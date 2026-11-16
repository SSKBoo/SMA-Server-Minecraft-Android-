package com.bedrockdroid.rs.model

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.os.Build
import android.os.PowerManager
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.atomic.AtomicLong

enum class EngineLane(val label: String) {
    BEDROCK("Bedrock"),
    JAVA("Java"),
    HYBRID("Híbrido")
}

enum class JavaFlavor(val label: String) {
    PAPER("Paper"),
    PURPUR("Purpur"),
    FABRIC("Fabric"),
    VANILLA("Vanilla")
}

enum class LogStream {
    CMD,
    DEBUG,
    STDOUT,
    WARN,
    ERR,
    NET
}

enum class LogLevel(val badge: String) {
    DEBUG("DEBUG"),
    INFO("INFO"),
    WARN("WARN"),
    ERROR("ERROR")
}

data class ConsoleLogLine(
    val id: Long,
    val time: String,
    val stream: LogStream,
    val text: String,
    val tag: String = "BedrockDroidUI",
    val level: LogLevel = when (stream) {
        LogStream.DEBUG -> LogLevel.DEBUG
        LogStream.WARN -> LogLevel.WARN
        LogStream.ERR -> LogLevel.ERROR
        LogStream.CMD, LogStream.STDOUT, LogStream.NET -> LogLevel.INFO
    }
)

data class HardwareScanReport(
    val socLabel: String,
    val detectedCores: Int,
    val primeMaxMhz: Int,
    val ramTotalMb: Int,
    val ramAvailMb: Int,
    val largeHeapClassMb: Int = 512,
    val storageFreeGb: Double,
    val recommendedXmsMb: Int,
    val recommendedXmxMb: Int,
    val recommendedRustRamMb: Int = 8192,
    val recommendedMtu: Int
)

data class NetworkProbeReport(
    val lanIp: String,
    val loopbackHint: String,
    val upnpStatus: String,
    val dualStackV6: Boolean,
    val isLikelyCgnat: Boolean,
    val guidance: String
)

data class SurvivalChecklistState(
    val notificationsEnabled: Boolean = false,
    val batteryOptimizationIgnored: Boolean = false,
    val backgroundDataUnrestricted: Boolean = false,
    val wakeLocksHeld: Boolean = false,
    val oemRuleConfirmed: Boolean = false,
    val lastCheckEpochMs: Long = 0L
) {
    val passedCount: Int
        get() = listOf(
            notificationsEnabled,
            batteryOptimizationIgnored,
            backgroundDataUnrestricted,
            wakeLocksHeld,
            oemRuleConfirmed
        ).count { it }

    val allPassed: Boolean
        get() = passedCount == 5
}

data class AndroidLauncherConfig(
    val serverName: String,
    val activeLane: EngineLane,
    val bedrockVersion: String,
    val bedrockPort: Int,
    val javaFlavor: JavaFlavor,
    val javaVersion: String,
    val javaPort: Int,
    val hybridGeyserVersion: String,
    val floodgateEnabled: Boolean,
    val viaVersionEnabled: Boolean,
    val jvmXmsMb: Int,
    val jvmXmxMb: Int,
    val rustMaxRamMb: Int,
    val eulaAccepted: Boolean,
    val localFastPath: Boolean,
    val tcpNoDelay: Boolean,
    val ipTosLowDelay: Boolean,
    val localJitterMs: Int,
    val wanMtuBytes: Int,
    val enableMdns: Boolean,
    val enableUpnp: Boolean,
    val dualStackIpv6: Boolean,
    val snapdragonPinning: Boolean,
    val ioPriorityBe: Boolean,
    val thermalGuardC: Int,
    val partialWakeLockEnabled: Boolean,
    val wifiLowLatencyLock: Boolean,
    val multicastLockEnabled: Boolean,
    val networkCallbackEnabled: Boolean,
    val oemAutostartConfirmed: Boolean
)

object LauncherPrefs {
    private const val PREFS_NAME = "bedrockdroid_rs_prefs"

    fun defaultConfig(): AndroidLauncherConfig = AndroidLauncherConfig(
        serverName = "BedrockDroid Survival ARM64",
        activeLane = EngineLane.valueOf("HYBRID"),
        bedrockVersion = "26.60",
        bedrockPort = 19132,
        javaFlavor = JavaFlavor.valueOf("PAPER"),
        javaVersion = "1.21.4",
        javaPort = 25565,
        hybridGeyserVersion = "2.6.0-b742",
        floodgateEnabled = true,
        viaVersionEnabled = true,
        jvmXmsMb = 1024,
        jvmXmxMb = 3072,
        rustMaxRamMb = 12288,
        eulaAccepted = true,
        localFastPath = true,
        tcpNoDelay = true,
        ipTosLowDelay = true,
        localJitterMs = 0,
        wanMtuBytes = 1400,
        enableMdns = true,
        enableUpnp = true,
        dualStackIpv6 = true,
        snapdragonPinning = true,
        ioPriorityBe = true,
        thermalGuardC = 43,
        partialWakeLockEnabled = true,
        wifiLowLatencyLock = true,
        multicastLockEnabled = true,
        networkCallbackEnabled = true,
        oemAutostartConfirmed = false
    )

    fun load(context: Context): AndroidLauncherConfig {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val def = defaultConfig()
        val lane = runCatching {
            EngineLane.valueOf(prefs.getString("activeLane", def.activeLane.name) ?: def.activeLane.name)
        }.getOrDefault(def.activeLane)
        val flavor = runCatching {
            JavaFlavor.valueOf(prefs.getString("javaFlavor", def.javaFlavor.name) ?: def.javaFlavor.name)
        }.getOrDefault(def.javaFlavor)

        return def.copy(
            serverName = prefs.getString("serverName", def.serverName) ?: def.serverName,
            activeLane = lane,
            bedrockVersion = prefs.getString("bedrockVersion", def.bedrockVersion) ?: def.bedrockVersion,
            bedrockPort = prefs.getInt("bedrockPort", def.bedrockPort),
            javaFlavor = flavor,
            javaVersion = prefs.getString("javaVersion", def.javaVersion) ?: def.javaVersion,
            javaPort = prefs.getInt("javaPort", def.javaPort),
            hybridGeyserVersion = prefs.getString("hybridGeyserVersion", def.hybridGeyserVersion)
                ?: def.hybridGeyserVersion,
            floodgateEnabled = prefs.getBoolean("floodgateEnabled", def.floodgateEnabled),
            viaVersionEnabled = prefs.getBoolean("viaVersionEnabled", def.viaVersionEnabled),
            jvmXmsMb = prefs.getInt("jvmXmsMb", def.jvmXmsMb),
            jvmXmxMb = prefs.getInt("jvmXmxMb", def.jvmXmxMb),
            rustMaxRamMb = prefs.getInt("rustMaxRamMb", def.rustMaxRamMb),
            eulaAccepted = prefs.getBoolean("eulaAccepted", def.eulaAccepted),
            localFastPath = prefs.getBoolean("localFastPath", def.localFastPath),
            tcpNoDelay = prefs.getBoolean("tcpNoDelay", def.tcpNoDelay),
            ipTosLowDelay = prefs.getBoolean("ipTosLowDelay", def.ipTosLowDelay),
            localJitterMs = prefs.getInt("localJitterMs", def.localJitterMs),
            wanMtuBytes = prefs.getInt("wanMtuBytes", def.wanMtuBytes),
            enableMdns = prefs.getBoolean("enableMdns", def.enableMdns),
            enableUpnp = prefs.getBoolean("enableUpnp", def.enableUpnp),
            dualStackIpv6 = prefs.getBoolean("dualStackIpv6", def.dualStackIpv6),
            snapdragonPinning = prefs.getBoolean("snapdragonPinning", def.snapdragonPinning),
            ioPriorityBe = prefs.getBoolean("ioPriorityBe", def.ioPriorityBe),
            thermalGuardC = prefs.getInt("thermalGuardC", def.thermalGuardC),
            partialWakeLockEnabled = prefs.getBoolean("partialWakeLockEnabled", def.partialWakeLockEnabled),
            wifiLowLatencyLock = prefs.getBoolean("wifiLowLatencyLock", def.wifiLowLatencyLock),
            multicastLockEnabled = prefs.getBoolean("multicastLockEnabled", def.multicastLockEnabled),
            networkCallbackEnabled = prefs.getBoolean("networkCallbackEnabled", def.networkCallbackEnabled),
            oemAutostartConfirmed = prefs.getBoolean("oemAutostartConfirmed", def.oemAutostartConfirmed)
        )
    }

    fun save(context: Context, cfg: AndroidLauncherConfig) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE).edit()
            .putString("serverName", cfg.serverName)
            .putString("activeLane", cfg.activeLane.name)
            .putString("bedrockVersion", cfg.bedrockVersion)
            .putInt("bedrockPort", cfg.bedrockPort)
            .putString("javaFlavor", cfg.javaFlavor.name)
            .putString("javaVersion", cfg.javaVersion)
            .putInt("javaPort", cfg.javaPort)
            .putString("hybridGeyserVersion", cfg.hybridGeyserVersion)
            .putBoolean("floodgateEnabled", cfg.floodgateEnabled)
            .putBoolean("viaVersionEnabled", cfg.viaVersionEnabled)
            .putInt("jvmXmsMb", cfg.jvmXmsMb)
            .putInt("jvmXmxMb", cfg.jvmXmxMb)
            .putInt("rustMaxRamMb", cfg.rustMaxRamMb)
            .putBoolean("eulaAccepted", cfg.eulaAccepted)
            .putBoolean("localFastPath", cfg.localFastPath)
            .putBoolean("tcpNoDelay", cfg.tcpNoDelay)
            .putBoolean("ipTosLowDelay", cfg.ipTosLowDelay)
            .putInt("localJitterMs", cfg.localJitterMs)
            .putInt("wanMtuBytes", cfg.wanMtuBytes)
            .putBoolean("enableMdns", cfg.enableMdns)
            .putBoolean("enableUpnp", cfg.enableUpnp)
            .putBoolean("dualStackIpv6", cfg.dualStackIpv6)
            .putBoolean("snapdragonPinning", cfg.snapdragonPinning)
            .putBoolean("ioPriorityBe", cfg.ioPriorityBe)
            .putInt("thermalGuardC", cfg.thermalGuardC)
            .putBoolean("partialWakeLockEnabled", cfg.partialWakeLockEnabled)
            .putBoolean("wifiLowLatencyLock", cfg.wifiLowLatencyLock)
            .putBoolean("multicastLockEnabled", cfg.multicastLockEnabled)
            .putBoolean("networkCallbackEnabled", cfg.networkCallbackEnabled)
            .putBoolean("oemAutostartConfirmed", cfg.oemAutostartConfirmed)
            .apply()
    }
}

data class ServerStatusSnapshot(
    val isRunning: Boolean = false,
    val locksAcquired: Boolean = false
)

object ServerRuntimeState {
    private val _serverStatus = MutableStateFlow(ServerStatusSnapshot())
    val serverStatus: StateFlow<ServerStatusSnapshot> = _serverStatus.asStateFlow()

    private val _isRunning = MutableStateFlow(false)
    val isRunning: StateFlow<Boolean> = _isRunning.asStateFlow()

    private val _locksAcquired = MutableStateFlow(false)
    val locksAcquired: StateFlow<Boolean> = _locksAcquired.asStateFlow()

    // Todos los ítems inicializan en PENDIENTE (false) hasta evaluar APIs reales de Android
    private val _checklistStatus = MutableStateFlow(SurvivalChecklistState())
    val checklistStatus: StateFlow<SurvivalChecklistState> = _checklistStatus.asStateFlow()

    private val _checklistRefreshTick = MutableStateFlow(0L)
    val checklistRefreshTick: StateFlow<Long> = _checklistRefreshTick.asStateFlow()

    private val _hardwareReport = MutableStateFlow<HardwareScanReport?>(null)
    val hardwareReport: StateFlow<HardwareScanReport?> = _hardwareReport.asStateFlow()

    private val _networkReport = MutableStateFlow<NetworkProbeReport?>(null)
    val networkReport: StateFlow<NetworkProbeReport?> = _networkReport.asStateFlow()

    private val _isScanningHardware = MutableStateFlow(false)
    val isScanningHardware: StateFlow<Boolean> = _isScanningHardware.asStateFlow()

    private val _isProbingNetwork = MutableStateFlow(false)
    val isProbingNetwork: StateFlow<Boolean> = _isProbingNetwork.asStateFlow()

    val logs: StateFlow<List<ConsoleLogLine>>
        get() = LogBuffer.logs

    fun setRunning(running: Boolean) {
        _isRunning.value = running
        val nextLocks = if (running) _serverStatus.value.locksAcquired else false
        _locksAcquired.value = nextLocks
        _serverStatus.value = ServerStatusSnapshot(
            isRunning = running,
            locksAcquired = nextLocks
        )
        _checklistStatus.update { current ->
            current.copy(
                wakeLocksHeld = running && nextLocks,
                lastCheckEpochMs = System.currentTimeMillis()
            )
        }
        _checklistRefreshTick.value += 1L
    }

    fun setLocksAcquired(acquired: Boolean) {
        val effectiveAcquired = if (_isRunning.value || _serverStatus.value.isRunning) acquired else false
        _locksAcquired.value = effectiveAcquired
        val running = _isRunning.value && _serverStatus.value.isRunning
        _serverStatus.value = _serverStatus.value.copy(locksAcquired = effectiveAcquired)
        _checklistStatus.update { current ->
            current.copy(
                wakeLocksHeld = running && effectiveAcquired,
                lastCheckEpochMs = System.currentTimeMillis()
            )
        }
        _checklistRefreshTick.value += 1L
    }

    fun setOemRuleConfirmed(confirmed: Boolean) {
        _checklistStatus.update { current ->
            current.copy(
                oemRuleConfirmed = confirmed,
                lastCheckEpochMs = System.currentTimeMillis()
            )
        }
        _checklistRefreshTick.value += 1L
    }

    /**
     * Evalúa siempre en vivo las APIs reales del sistema Android sin leer SharedPreferences cacheadas
     * y fuerza la emisión de todos los StateFlow para refrescar la UI de inmediato.
     */
    fun refreshChecklist(context: Context) {
        val notifManagerOk = runCatching {
            NotificationManagerCompat.from(context).areNotificationsEnabled()
        }.getOrDefault(false)
        val notifPermOk = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED
        } else {
            true
        }
        val notifOk = notifManagerOk && notifPermOk

        val batteryOk = runCatching {
            val pm = context.getSystemService(Context.POWER_SERVICE) as? PowerManager
            pm?.isIgnoringBatteryOptimizations(context.packageName) == true
        }.getOrDefault(false)

        val bgDataOk = runCatching {
            val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            cm != null && cm.restrictBackgroundStatus != ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED
        }.getOrDefault(false)

        // Datos en segundo plano solo pasa si notificaciones y exención Doze también están concedidas
        val backgroundDataUnrestricted = notifOk && batteryOk && bgDataOk

        val runningNow = _isRunning.value && _serverStatus.value.isRunning
        val locksNow = if (runningNow) {
            _locksAcquired.value && _serverStatus.value.locksAcquired
        } else {
            false
        }

        _isRunning.value = runningNow
        _locksAcquired.value = locksNow
        _serverStatus.value = ServerStatusSnapshot(
            isRunning = runningNow,
            locksAcquired = locksNow
        )

        _checklistStatus.value = SurvivalChecklistState(
            notificationsEnabled = notifOk,
            batteryOptimizationIgnored = batteryOk,
            backgroundDataUnrestricted = backgroundDataUnrestricted,
            wakeLocksHeld = runningNow && locksNow,
            oemRuleConfirmed = _checklistStatus.value.oemRuleConfirmed,
            lastCheckEpochMs = System.currentTimeMillis()
        )
        _checklistRefreshTick.value += 1L
    }

    fun setScanningHardware(scanning: Boolean) {
        _isScanningHardware.value = scanning
    }

    fun updateHardwareReport(report: HardwareScanReport) {
        _hardwareReport.value = report
        _isScanningHardware.value = false
    }

    fun setProbingNetwork(probing: Boolean) {
        _isProbingNetwork.value = probing
    }

    fun updateNetworkReport(report: NetworkProbeReport) {
        _networkReport.value = report
        _isProbingNetwork.value = false
    }

    fun appendLog(stream: LogStream, text: String, tag: String = "BedrockDroidUI") {
        LogBuffer.append(stream = stream, text = text, tag = tag)
    }

    fun clearLogs() {
        LogBuffer.clear()
    }
}
