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

class ServerForegroundService : Service() {

    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var multicastLock: WifiManager.MulticastLock? = null
    private var lanDiscovery: LanDiscoveryManager? = null
    private var networkMonitor: NetworkMonitor? = null

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

        acquireSurvivalLocks(cfg)

        val laneDir = EngineLaunchPlanner.laneDirectory(filesDir, cfg.activeLane)
        EngineLaunchPlanner.ensureEula(laneDir, cfg.eulaAccepted)
        if (cfg.activeLane == EngineLane.HYBRID) {
            EngineLaunchPlanner.createOrUpdateHybridProfile(filesDir, cfg)
        }

        lanDiscovery = LanDiscoveryManager(this).apply {
            if (cfg.enableMdns) {
                runCatching { registerLanService(cfg.serverName, cfg.bedrockPort) }
                    .onSuccess {
                        ServerRuntimeState.appendLog(
                            LogStream.NET,
                            "[mDNS] Anunciando _minecraft._udp en puerto ${cfg.bedrockPort}"
                        )
                    }
            }
        }

        networkMonitor = NetworkMonitor(this).apply {
            if (cfg.networkCallbackEnabled) {
                startObserving {
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
            cfg.localFastPath,
            cfg.tcpNoDelay,
            cfg.ipTosLowDelay,
            cfg.localJitterMs,
            cfg.wanMtuBytes,
            cfg.snapdragonPinning,
            cfg.ioPriorityBe,
            cfg.thermalGuardC
        )

        ServerRuntimeState.setRunning(true)
        ServerRuntimeState.appendLog(
            LogStream.STDOUT,
            "[Servicio 24/7] Carril ${cfg.activeLane.label} iniciado en ${laneDir.absolutePath}"
        )
        val rustStream = if (rustStatus.startsWith("OK")) LogStream.STDOUT else LogStream.ERR
        ServerRuntimeState.appendLog(rustStream, "[Rust JNI] $rustStatus", "RustJNI")
        if (cfg.activeLane != EngineLane.BEDROCK) {
            ServerRuntimeState.appendLog(
                LogStream.STDOUT,
                "[JVM ARM64] ${EngineLaunchPlanner.buildJvmArgs(cfg).joinToString(" ")}"
            )
        }
        ServerRuntimeState.appendLog(
            LogStream.NET,
            "[Fast-Path Local] Juega en este celular en 127.0.0.1:$activePort (<1 ms, jitter=0ms)"
        )

        return START_STICKY
    }

    private fun acquireSurvivalLocks(cfg: AndroidLauncherConfig) {
        if (cfg.partialWakeLockEnabled) {
            val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "BedrockDroidRS::ServerWakeLock").apply {
                setReferenceCounted(false)
                if (!isHeld) acquire()
            }
        }

        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        if (cfg.wifiLowLatencyLock) {
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

        if (cfg.multicastLockEnabled) {
            multicastLock = wifi.createMulticastLock("BedrockDroidRS::MdnsMulticast").apply {
                setReferenceCounted(false)
                if (!isHeld) acquire()
            }
        }
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
        runCatching { lanDiscovery?.unregister() }
        runCatching { networkMonitor?.stopObserving() }
        runCatching { if (multicastLock?.isHeld == true) multicastLock?.release() }
        runCatching { if (wifiLock?.isHeld == true) wifiLock?.release() }
        runCatching { if (wakeLock?.isHeld == true) wakeLock?.release() }
        RustCoreBridge.shutdownRuntimeSafe()
        ServerRuntimeState.setRunning(false)
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
