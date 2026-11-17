package com.bedrockdroid.rs.nativebridge

import com.bedrockdroid.rs.crash.CrashHandler

object RustCoreBridge {
    fun interface PlayerConnectionListener {
        fun onPlayerLoginStateChanged(playerName: String, connectedCount: Int, connected: Boolean)
    }

    fun interface CrashCallbackListener {
        fun onRustCrash(error: String)
    }

    @Volatile
    private var nativeLoaded: Boolean = false

    @Volatile
    private var loadError: String? = null

    init {
        try {
            System.loadLibrary("bedrock_android_core")
            nativeLoaded = true
        } catch (t: Throwable) {
            nativeLoaded = false
            loadError = "${t.javaClass.simpleName}: ${t.message}"
        }
    }

    fun isAvailable(): Boolean = nativeLoaded

    fun lastLoadError(): String? = loadError

    fun registerCrashCallbackSafe(
        crashLogPath: String,
        enabled: Boolean = false,
        listener: CrashCallbackListener = CrashCallbackListener { error -> CrashHandler.logCrash(error) }
    ): Boolean {
        if (!nativeLoaded) return false
        return try {
            registerCrashCallback(crashLogPath, listener)
            setCrashProtectionEnabled(enabled)
            true
        } catch (_: Throwable) {
            false
        }
    }

    fun setCrashProtectionEnabledSafe(enabled: Boolean): Boolean {
        if (!nativeLoaded) return false
        return try {
            setCrashProtectionEnabled(enabled)
            true
        } catch (_: Throwable) {
            false
        }
    }

    fun registerPlayerCallbackSafe(listener: PlayerConnectionListener): Boolean {
        if (!nativeLoaded) return false
        return try {
            registerPlayerCallback(listener)
            true
        } catch (_: Throwable) {
            false
        }
    }

    fun pollConnectedPlayerStatusSafe(): String {
        if (!nativeLoaded) {
            return "{\"connected\":false,\"count\":0,\"player\":\"\",\"worldSpawnSent\":false,\"chunksSent\":0}"
        }
        return try {
            pollConnectedPlayerStatus()
        } catch (_: Throwable) {
            "{\"connected\":false,\"count\":0,\"player\":\"\",\"worldSpawnSent\":false,\"chunksSent\":0}"
        }
    }

    fun initRuntimeSafe(
        engineDir: String,
        serverName: String,
        laneName: String,
        bedrockPort: Int,
        maxRamMb: Int,
        blockingThreads: Int,
        localFastPath: Boolean,
        tcpNoDelay: Boolean,
        ipTosLowDelay: Boolean,
        localJitterMs: Int,
        wanMtuBytes: Int,
        snapdragonPinning: Boolean,
        ioPriorityBestEffort: Boolean,
        thermalGuardC: Int
    ): String {
        if (!nativeLoaded) {
            return "JNI no disponible (${loadError ?: "librería nativa no cargada"})"
        }
        return try {
            initRuntime(
                engineDir,
                serverName,
                laneName,
                bedrockPort,
                maxRamMb,
                blockingThreads,
                localFastPath,
                tcpNoDelay,
                ipTosLowDelay,
                localJitterMs,
                wanMtuBytes,
                snapdragonPinning,
                ioPriorityBestEffort,
                thermalGuardC
            )
        } catch (t: Throwable) {
            "Error JNI initRuntime: ${t.javaClass.simpleName}: ${t.message}"
        }
    }

    fun probeNetworkAndUpnpSafe(
        bedrockPort: Int,
        javaPort: Int,
        enableUpnp: Boolean,
        dualStackIpv6: Boolean
    ): String? {
        if (!nativeLoaded) return null
        return try {
            probeNetworkAndUpnp(bedrockPort, javaPort, enableUpnp, dualStackIpv6)
        } catch (_: Throwable) {
            null
        }
    }

    fun readSoCTopologyJsonSafe(): String {
        if (!nativeLoaded) return "{}"
        return try {
            readSoCTopologyJson()
        } catch (_: Throwable) {
            "{}"
        }
    }

    fun sendPingCommandSafe(bedrockPort: Int = 19132): String {
        if (!nativeLoaded) {
            return "[PING] JNI no disponible (${loadError ?: "librería nativa no cargada"})"
        }
        return try {
            sendPingCommand(bedrockPort)
        } catch (t: Throwable) {
            "[PING] Error JNI sendPingCommand: ${t.javaClass.simpleName}: ${t.message}"
        }
    }

    fun shutdownRuntimeSafe() {
        if (!nativeLoaded) return
        try {
            shutdownRuntime()
        } catch (_: Throwable) {
            // Ignorar fallos durante apagado nativo
        }
    }

    external fun registerCrashCallback(crashLogPath: String, listener: CrashCallbackListener)

    external fun setCrashProtectionEnabled(enabled: Boolean)

    external fun registerPlayerCallback(listener: PlayerConnectionListener)

    external fun pollConnectedPlayerStatus(): String

    external fun initRuntime(
        engineDir: String,
        serverName: String,
        laneName: String,
        bedrockPort: Int,
        maxRamMb: Int,
        blockingThreads: Int,
        localFastPath: Boolean,
        tcpNoDelay: Boolean,
        ipTosLowDelay: Boolean,
        localJitterMs: Int,
        wanMtuBytes: Int,
        snapdragonPinning: Boolean,
        ioPriorityBestEffort: Boolean,
        thermalGuardC: Int
    ): String

    external fun probeNetworkAndUpnp(
        bedrockPort: Int,
        javaPort: Int,
        enableUpnp: Boolean,
        dualStackIpv6: Boolean
    ): String

    external fun readSoCTopologyJson(): String

    external fun sendPingCommand(bedrockPort: Int): String

    external fun shutdownRuntime()
}
