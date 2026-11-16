package com.bedrockdroid.rs.nativebridge

object RustCoreBridge {
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

    fun initRuntimeSafe(
        engineDir: String,
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

    fun shutdownRuntimeSafe() {
        if (!nativeLoaded) return
        try {
            shutdownRuntime()
        } catch (_: Throwable) {
            // Ignorar fallos durante apagado nativo
        }
    }

    external fun initRuntime(
        engineDir: String,
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

    external fun shutdownRuntime()
}
