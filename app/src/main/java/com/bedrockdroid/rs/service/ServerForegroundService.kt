package com.bedrockdroid.rs.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import com.bedrockdroid.rs.engine.EngineLaunchPlanner
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.EngineLane
import com.bedrockdroid.rs.model.LauncherPrefs
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.ServerRuntimeState
import com.bedrockdroid.rs.nativebridge.RustCoreBridge
import com.bedrockdroid.rs.net.LanDiscoveryManager
import com.bedrockdroid.rs.net.NetworkMonitor
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

class ServerForegroundService : Service() {

    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var multicastLock: WifiManager.MulticastLock? = null
    private var lanDiscovery: LanDiscoveryManager? = null
    private var networkMonitor: NetworkMonitor? = null
    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    @Volatile
    private var rustRuntimeStarted: Boolean = false

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        LogBuffer.startProcessLogcatCollector()
        val cfg = LauncherPrefs.load(this)
        ensureNotificationChannel()

        val activePort = if (cfg.activeLane == EngineLane.JAVA) cfg.javaPort else cfg.bedrockPort
        val notification = buildNotification(
            cfg.serverName,
            "Carril ${cfg.activeLane.label} activo · 127.0.0.1:$activePort"
        )

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ServiceCompat.startForeground(
                this,
                NOTIF_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
            )
        } else {
            startForeground(NOTIF_ID, notification)
        }

        ServerRuntimeState.setRunning(true)
        ServerRuntimeState.setLocksAcquired(false)
        val locksOk = acquireSurvivalLocks(cfg)
        ServerRuntimeState.setLocksAcquired(locksOk)

        // Actualizar hardware y red en segundo plano para la Tarjeta Superior en tiempo real
        ServerRuntimeState.setScanningHardware(true)
        ServerRuntimeState.setProbingNetwork(true)
        serviceScope.launch {
            runCatching {
                val hw = EngineLaunchPlanner.scanDeviceHardware(applicationContext)
                ServerRuntimeState.updateHardwareReport(hw)
            }.onFailure {
                ServerRuntimeState.setScanningHardware(false)
            }
            runCatching {
                val net = EngineLaunchPlanner.probeNetwork(cfg)
                ServerRuntimeState.updateNetworkReport(net)
            }.onFailure {
                ServerRuntimeState.setProbingNetwork(false)
            }
        }

        val laneDir = EngineLaunchPlanner.laneDirectory(filesDir, cfg.activeLane)
        EngineLaunchPlanner.ensureEula(laneDir, cfg.eulaAccepted)

        // Reiniciar monitores previos antes de levantar el carril seleccionado
        runCatching { lanDiscovery?.unregister() }
        lanDiscovery = null
        runCatching { networkMonitor?.stopObserving() }
        networkMonitor = null
        rustRuntimeStarted = false

        when (cfg.activeLane) {
            EngineLane.BEDROCK -> {
                // Carril Bedrock: SOLO motor nativo Rust + RakNet/mDNS UDP. NO iniciar JVM.
                if (cfg.enableMdns) {
                    lanDiscovery = LanDiscoveryManager(this).apply {
                        runCatching { registerLanService(cfg.serverName, cfg.bedrockPort) }
                            .onSuccess {
                                ServerRuntimeState.appendLog(
                                    LogStream.NET,
                                    "[mDNS] Anunciando _minecraft._udp en puerto ${cfg.bedrockPort}"
                                )
                            }
                            .onFailure { err ->
                                ServerRuntimeState.appendLog(
                                    LogStream.WARN,
                                    "[mDNS] No se pudo iniciar anuncio NsdManager: ${err.message}"
                                )
                            }
                    }
                }

                if (cfg.networkCallbackEnabled) {
                    networkMonitor = NetworkMonitor(this).apply {
                        startObserving {
                            val probe = EngineLaunchPlanner.probeNetwork(cfg)
                            ServerRuntimeState.updateNetworkReport(probe)
                            val probeJson = RustCoreBridge.probeNetworkAndUpnpSafe(
                                cfg.bedrockPort,
                                cfg.javaPort,
                                cfg.enableUpnp,
                                cfg.dualStackIpv6
                            )
                            if (probeJson != null) {
                                ServerRuntimeState.appendLog(LogStream.NET, "[NetworkCallback] $probeJson")
                            }
                        }
                    }
                }

                val rustStatus = RustCoreBridge.initRuntimeSafe(
                    laneDir.absolutePath,
                    cfg.serverName,
                    cfg.activeLane.name,
                    cfg.bedrockPort,
                    cfg.rustMaxRamMb,
                    cfg.localFastPath,
                    cfg.tcpNoDelay,
                    cfg.ipTosLowDelay,
                    cfg.localJitterMs,
                    cfg.wanMtuBytes,
                    cfg.snapdragonPinning,
                    cfg.ioPriorityBe,
                    cfg.thermalGuardC
                )
                rustRuntimeStarted = rustStatus.startsWith("OK")
                val rustStream = if (rustRuntimeStarted) LogStream.STDOUT else LogStream.ERR
                ServerRuntimeState.appendLog(rustStream, "[Rust JNI] $rustStatus", "RustJNI")
            }

            EngineLane.JAVA -> {
                // Carril Java: SOLO JVM ARM64 en TCP 25565. NO invocar initRuntime ni RakNet/mDNS UDP 19132.
                val jvmArgs = EngineLaunchPlanner.buildJvmArgs(cfg).joinToString(" ")
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[JVM ARM64] Iniciando perfil Java (${cfg.javaFlavor.label} ${cfg.javaVersion}) en TCP ${cfg.javaPort}"
                )
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[JVM ARM64] $jvmArgs"
                )
            }

            EngineLane.HYBRID -> {
                // Carril Híbrido: inicia AMBOS -> JVM ARM64 (TCP 25565) + motor Bedrock/Geyser Rust JNI (UDP 19132).
                EngineLaunchPlanner.createOrUpdateHybridProfile(filesDir, cfg)
                val jvmArgs = EngineLaunchPlanner.buildJvmArgs(cfg).joinToString(" ")
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[JVM ARM64] Iniciando base Java (${cfg.javaFlavor.label} ${cfg.javaVersion} + Geyser ${cfg.hybridGeyserVersion}) en TCP ${cfg.javaPort}"
                )
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[JVM ARM64] $jvmArgs"
                )

                if (cfg.enableMdns) {
                    lanDiscovery = LanDiscoveryManager(this).apply {
                        runCatching { registerLanService(cfg.serverName, cfg.bedrockPort) }
                            .onSuccess {
                                ServerRuntimeState.appendLog(
                                    LogStream.NET,
                                    "[mDNS] Anunciando _minecraft._udp en puerto ${cfg.bedrockPort}"
                                )
                            }
                            .onFailure { err ->
                                ServerRuntimeState.appendLog(
                                    LogStream.WARN,
                                    "[mDNS] No se pudo iniciar anuncio NsdManager: ${err.message}"
                                )
                            }
                    }
                }

                if (cfg.networkCallbackEnabled) {
                    networkMonitor = NetworkMonitor(this).apply {
                        startObserving {
                            val probe = EngineLaunchPlanner.probeNetwork(cfg)
                            ServerRuntimeState.updateNetworkReport(probe)
                            val probeJson = RustCoreBridge.probeNetworkAndUpnpSafe(
                                cfg.bedrockPort,
                                cfg.javaPort,
                                cfg.enableUpnp,
                                cfg.dualStackIpv6
                            )
                            if (probeJson != null) {
                                ServerRuntimeState.appendLog(LogStream.NET, "[NetworkCallback] $probeJson")
                            }
                        }
                    }
                }

                val rustStatus = RustCoreBridge.initRuntimeSafe(
                    laneDir.absolutePath,
                    cfg.serverName,
                    cfg.activeLane.name,
                    cfg.bedrockPort,
                    cfg.rustMaxRamMb,
                    cfg.localFastPath,
                    cfg.tcpNoDelay,
                    cfg.ipTosLowDelay,
                    cfg.localJitterMs,
                    cfg.wanMtuBytes,
                    cfg.snapdragonPinning,
                    cfg.ioPriorityBe,
                    cfg.thermalGuardC
                )
                rustRuntimeStarted = rustStatus.startsWith("OK")
                val rustStream = if (rustRuntimeStarted) LogStream.STDOUT else LogStream.ERR
                ServerRuntimeState.appendLog(rustStream, "[Rust JNI] $rustStatus", "RustJNI")
            }
        }

        ServerRuntimeState.setRunning(true)
        ServerRuntimeState.refreshChecklist(applicationContext)
        ServerRuntimeState.appendLog(
            LogStream.STDOUT,
            "[Servicio 24/7] Carril ${cfg.activeLane.label} iniciado en ${laneDir.absolutePath}"
        )
        ServerRuntimeState.appendLog(
            LogStream.NET,
            "[Fast-Path Local] Juega en este celular en 127.0.0.1:$activePort (<1 ms, jitter=0ms)"
        )

        return START_STICKY
    }

    private fun acquireSurvivalLocks(cfg: AndroidLauncherConfig): Boolean {
        if (cfg.partialWakeLockEnabled) {
            runCatching {
                val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
                wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "BedrockDroidRS::ServerWakeLock").apply {
                    setReferenceCounted(false)
                    if (!isHeld) acquire()
                }
            }
        }

        val wifi = runCatching { applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager }.getOrNull()
        if (wifi != null && cfg.wifiLowLatencyLock) {
            runCatching {
                val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    WifiManager.WIFI_MODE_FULL_LOW_LATENCY
                } else {
                    @Suppress("DEPRECATION")
                    WifiManager.WIFI_MODE_FULL_HIGH_PERF
                }
                wifiLock = wifi.createWifiLock(mode, "BedrockDroidRS::LowLatencyWifi").apply {
                    setReferenceCounted(false)
                    if (!isHeld) acquire()
                }
            }
        }

        if (wifi != null && cfg.multicastLockEnabled) {
            runCatching {
                multicastLock = wifi.createMulticastLock("BedrockDroidRS::MdnsMulticast").apply {
                    setReferenceCounted(false)
                    if (!isHeld) acquire()
                }
            }
        }

        // Solo PASS cuando los tres locks (WakeLock, WifiLock y MulticastLock) están activamente adquiridos
        val wakeOk = wakeLock?.isHeld == true
        val wifiOk = wifiLock?.isHeld == true
        val multiOk = multicastLock?.isHeld == true
        val allAcquired = wakeOk && wifiOk && multiOk
        ServerRuntimeState.appendLog(
            if (allAcquired) LogStream.STDOUT else LogStream.WARN,
            "[Locks 24/7] WakeLock=$wakeOk · WifiLock=$wifiOk · Multicast=$multiOk (all=$allAcquired)"
        )
        return allAcquired
    }

    private fun ensureNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Servidor Minecraft 24/7",
                NotificationManager.IMPORTANCE_LOW
            )
            nm.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(title: String, statusText: String): Notification {
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(title)
            .setContentText(statusText)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setOngoing(true)
            .build()
    }

    override fun onDestroy() {
        runCatching { serviceScope.cancel() }
        runCatching { lanDiscovery?.unregister() }
        lanDiscovery = null
        runCatching { networkMonitor?.stopObserving() }
        networkMonitor = null
        runCatching { if (multicastLock?.isHeld == true) multicastLock?.release() }
        multicastLock = null
        runCatching { if (wifiLock?.isHeld == true) wifiLock?.release() }
        wifiLock = null
        runCatching { if (wakeLock?.isHeld == true) wakeLock?.release() }
        wakeLock = null
        if (rustRuntimeStarted) {
            RustCoreBridge.shutdownRuntimeSafe()
            rustRuntimeStarted = false
        }
        ServerRuntimeState.setLocksAcquired(false)
        ServerRuntimeState.setRunning(false)
        ServerRuntimeState.refreshChecklist(applicationContext)
        ServerRuntimeState.appendLog(LogStream.WARN, "[Servicio 24/7] Servidor detenido y locks liberados.")
        LogBuffer.stopProcessLogcatCollector()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        private const val CHANNEL_ID = "bedrockdroid_server_247"
        private const val NOTIF_ID = 19132
    }
}
