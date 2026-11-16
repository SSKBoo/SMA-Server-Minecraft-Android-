package com.bedrockdroid.rs.engine

import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.StatFs
import androidx.core.content.ContextCompat
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.EngineLane
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.JavaFlavor
import com.bedrockdroid.rs.model.LauncherPrefs
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.NetworkProbeReport
import com.bedrockdroid.rs.model.ServerRuntimeState
import com.bedrockdroid.rs.nativebridge.RustCoreBridge
import com.bedrockdroid.rs.service.ServerForegroundService
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.Inet4Address
import java.net.NetworkInterface
import java.net.URL

object EngineLaunchPlanner {
    fun officialEndpoints(javaVersion: String): Map<String, String> = mapOf(
        "paper" to "https://api.papermc.io/v2/projects/paper/versions/$javaVersion",
        "purpur" to "https://api.purpurmc.org/v2/purpur/$javaVersion",
        "fabric" to "https://meta.fabricmc.net/v2/versions/loader/$javaVersion",
        "vanilla" to "https://launchermeta.mojang.com/mc/game/version_manifest_v2.json",
        "geyser" to "https://download.geysermc.org/v2/projects/geyser/versions/latest/builds/latest",
        "floodgate" to "https://download.geysermc.org/v2/projects/floodgate/versions/latest/builds/latest"
    )

    fun laneDirectory(baseFilesDir: File, lane: EngineLane): File {
        val dir = File(baseFilesDir, "engines/${lane.name.lowercase()}")
        if (!dir.exists()) dir.mkdirs()
        return dir
    }

    fun ensureEula(laneDir: File, accepted: Boolean) {
        File(laneDir, "eula.txt").writeText("eula=$accepted\n")
    }

    fun createOrUpdateHybridProfile(baseFilesDir: File, cfg: AndroidLauncherConfig): File {
        val hybridDir = laneDirectory(baseFilesDir, EngineLane.HYBRID)
        ensureEula(hybridDir, cfg.eulaAccepted)
        val meta = JSONObject()
            .put("base_java_flavor", cfg.javaFlavor.name.lowercase())
            .put("base_java_version", cfg.javaVersion)
            .put("geyser_version", cfg.hybridGeyserVersion)
            .put("floodgate_enabled", cfg.floodgateEnabled)
            .put("viaversion_enabled", cfg.viaVersionEnabled)
            .put("bedrock_udp_port", cfg.bedrockPort)
            .put("java_tcp_port", cfg.javaPort)
        File(hybridDir, "hybrid-profile.json").writeText(meta.toString(2))
        return hybridDir
    }

    fun buildJvmArgs(cfg: AndroidLauncherConfig): List<String> {
        val gcFlags = listOf("-XX:+UseG1GC", "-XX:+UseStringDeduplication", "-XX:MaxGCPauseMillis=50")
        return listOf(
            "-Xms${cfg.jvmXmsMb}M",
            "-Xmx${cfg.jvmXmxMb}M",
            "-XX:+AlwaysPreTouch",
            "-Dusing.aikars.flags=mobile-arm64"
        ) + gcFlags
    }

    fun scanDeviceHardware(context: Context): HardwareScanReport {
        ServerRuntimeState.setScanningHardware(true)
        val rawJson = RustCoreBridge.readSoCTopologyJsonSafe()
        val obj = runCatching { JSONObject(rawJson) }.getOrNull()
        val cores = obj?.optInt("detected_cores", Runtime.getRuntime().availableProcessors())
            ?: Runtime.getRuntime().availableProcessors()
        val primeKhz = obj?.optInt("prime_max_khz", 2800000) ?: 2800000
        val primeMhz = primeKhz / 1000

        val am = context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
        val memInfo = ActivityManager.MemoryInfo()
        am.getMemoryInfo(memInfo)
        val totalMb = (memInfo.totalMem / (1024L * 1024L)).toInt()
        val availMb = (memInfo.availMem / (1024L * 1024L)).toInt()
        val largeHeapMb = am.largeMemoryClass.coerceAtLeast(256)

        val stat = StatFs(context.filesDir.absolutePath)
        val freeGb = stat.availableBytes.toDouble() / (1024.0 * 1024.0 * 1024.0)

        // La JVM dentro del proceso Android está estrictamente limitada por getLargeMemoryClass()
        val safeMaxXmx = largeHeapMb
        val recXms = (safeMaxXmx / 2).coerceIn(128, safeMaxXmx)
        val maxRustBudget = (((totalMb * 0.85).toInt() / 512) * 512).coerceAtLeast(1024)
        val recRustMb = (((totalMb * 0.75).toInt() / 512) * 512).coerceIn(1024, maxRustBudget)
        val recMtu = if (totalMb >= 8000) 1400 else 1360

        val report = HardwareScanReport(
            socLabel = "${Build.MANUFACTURER.replaceFirstChar { it.uppercase() }} ${Build.MODEL} (${Build.HARDWARE})",
            detectedCores = cores,
            primeMaxMhz = primeMhz,
            ramTotalMb = totalMb,
            ramAvailMb = availMb,
            largeHeapClassMb = largeHeapMb,
            storageFreeGb = freeGb,
            recommendedXmsMb = recXms,
            recommendedXmxMb = safeMaxXmx,
            recommendedRustRamMb = recRustMb,
            recommendedMtu = recMtu
        )
        ServerRuntimeState.updateHardwareReport(report)
        return report
    }

    fun probeNetwork(cfg: AndroidLauncherConfig): NetworkProbeReport {
        ServerRuntimeState.setProbingNetwork(true)
        val raw = RustCoreBridge.probeNetworkAndUpnpSafe(
            cfg.bedrockPort,
            cfg.javaPort,
            cfg.enableUpnp,
            cfg.dualStackIpv6
        ) ?: "{}"
        val obj = runCatching { JSONObject(raw) }.getOrNull()
        val upnp = obj?.optString("upnp", "no_igd_or_cgnat") ?: "no_igd_or_cgnat"

        val lanIp = findLocalLanIpv4() ?: "127.0.0.1"
        val isCgnatOrClosed = upnp != "igd_gateway_found"
        val port = if (cfg.activeLane == EngineLane.JAVA) cfg.javaPort else cfg.bedrockPort

        val guidance = when {
            upnp == "igd_gateway_found" ->
                "En este móvil juega por 127.0.0.1:$port (<1 ms). En tu Wi-Fi LAN aparece en $lanIp:$port y router UPnP listo."
            lanIp == "127.0.0.1" ->
                "Sin Wi-Fi activo (datos móviles o modo local). El operador móvil suele usar CGNAT; usa 127.0.0.1:$port en local o túnel para amigos."
            else ->
                "Local 127.0.0.1:$port y LAN $lanIp:$port listos. El router no respondió a UPnP IGD (verifica redirección manual o CGNAT)."
        }

        val report = NetworkProbeReport(
            lanIp = lanIp,
            loopbackHint = "127.0.0.1:$port",
            upnpStatus = upnp,
            dualStackV6 = cfg.dualStackIpv6,
            isLikelyCgnat = isCgnatOrClosed,
            guidance = guidance
        )
        ServerRuntimeState.updateNetworkReport(report)
        return report
    }

    fun checkEndpointReachable(urlString: String): String {
        return runCatching {
            val conn = (URL(urlString).openConnection() as HttpURLConnection).apply {
                requestMethod = "GET"
                connectTimeout = 4000
                readTimeout = 4000
                setRequestProperty("User-Agent", "BedrockDroidRS/1.0")
            }
            val code = conn.responseCode
            conn.disconnect()
            "HTTP $code OK"
        }.getOrElse { err ->
            "Sin conexión (${err.javaClass.simpleName})"
        }
    }

    private fun findLocalLanIpv4(): String? {
        return runCatching {
            val ifaces = NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
            for (iface in ifaces) {
                if (!iface.isUp || iface.isLoopback) continue
                for (addr in iface.inetAddresses.toList()) {
                    if (addr is Inet4Address && !addr.isLoopbackAddress && addr.isSiteLocalAddress) {
                        return addr.hostAddress
                    }
                }
            }
            null
        }.getOrNull()
    }

    fun executeCliCommand(
        context: Context,
        rawCommand: String,
        cfg: AndroidLauncherConfig,
        onConfigChange: (AndroidLauncherConfig) -> Unit
    ) {
        val cmd = rawCommand.trim()
        if (cmd.isEmpty()) return
        val lower = cmd.lowercase()

        if (lower == "clear" || lower == "cls") {
            ServerRuntimeState.clearLogs()
            return
        }

        ServerRuntimeState.appendLog(LogStream.CMD, "\$ $cmd")

        when {
            lower == "help" || lower == "?" -> {
                ServerRuntimeState.appendLog(LogStream.STDOUT, "Comandos disponibles:")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  ./launch bedrock | java | hybrid -> Inicia el carril elegido")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  stop                             -> Detiene ForegroundService")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  ping | test                      -> Envía Unconnected Ping RakNet a 127.0.0.1")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  netcheck | upnp status           -> Prueba LAN, loopback y UPnP")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  versions                         -> Lista rutas en /files/engines")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  convert hybrid                   -> Crea perfil Híbrido aparte")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  soc                              -> Lee topología ARM64 vía JNI")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  clear                            -> Limpia la consola")
            }
            lower == "ping" || lower == "test" -> {
                val pingResponse = RustCoreBridge.sendPingCommandSafe(cfg.bedrockPort)
                val stream = if (pingResponse.contains("responde", ignoreCase = true)) {
                    LogStream.NET
                } else {
                    LogStream.WARN
                }
                ServerRuntimeState.appendLog(stream, pingResponse, "RustPing")
            }
            lower == "stop" -> {
                context.stopService(Intent(context, ServerForegroundService::class.java))
            }
            lower == "versions" -> {
                EngineLane.entries.forEach { lane ->
                    val dir = laneDirectory(context.filesDir, lane)
                    ServerRuntimeState.appendLog(LogStream.STDOUT, "  [${lane.label}] ${dir.absolutePath}")
                }
            }
            lower == "soc" -> {
                val hw = scanDeviceHardware(context)
                val totalGb = ((hw.ramTotalMb + 512) / 1024).coerceAtLeast(1)
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[SoC] ${hw.detectedCores} núcleos · Prime ${hw.primeMaxMhz} MHz · RAM física $totalGb GB totales (${hw.ramTotalMb} MB · libre ${hw.ramAvailMb} MB) · largeHeap ${hw.largeHeapClassMb} MB"
                )
            }
            lower == "convert hybrid" -> {
                val dir = createOrUpdateHybridProfile(context.filesDir, cfg)
                onConfigChange(cfg.copy(activeLane = EngineLane.HYBRID))
                ServerRuntimeState.appendLog(LogStream.STDOUT, "[Convertidor] Perfil Híbrido guardado en ${dir.absolutePath}")
            }
            lower == "netcheck" || lower == "upnp status" -> {
                val probe = probeNetwork(cfg)
                ServerRuntimeState.appendLog(
                    LogStream.NET,
                    "[Net] Loopback=${probe.loopbackHint} · LAN=${probe.lanIp} · UPnP=${probe.upnpStatus}"
                )
                ServerRuntimeState.appendLog(LogStream.STDOUT, probe.guidance)
            }
            lower == "./launch bedrock" || lower == "./launch java" || lower == "./launch hybrid" || lower.startsWith("java ") -> {
                val targetLane = when {
                    lower.contains("bedrock") -> EngineLane.BEDROCK
                    lower.contains("java") -> EngineLane.JAVA
                    else -> EngineLane.HYBRID
                }
                if (ServerRuntimeState.isRunning.value && targetLane != cfg.activeLane) {
                    ServerRuntimeState.appendLog(
                        LogStream.WARN,
                        "Detén el servidor antes de cambiar de perfil (ejecuta 'stop' primero)."
                    )
                    return
                }
                val updated = cfg.copy(activeLane = targetLane)
                LauncherPrefs.save(context, updated)
                onConfigChange(updated)
                if (targetLane == EngineLane.JAVA || targetLane == EngineLane.HYBRID) {
                    ServerRuntimeState.appendLog(
                        LogStream.STDOUT,
                        "[JVM] ${buildJvmArgs(updated).joinToString(" ")}"
                    )
                }
                ContextCompat.startForegroundService(
                    context,
                    Intent(context, ServerForegroundService::class.java)
                )
            }
            else -> {
                ServerRuntimeState.appendLog(
                    LogStream.ERR,
                    "Comando no reconocido: '$cmd'. Escribe 'help'."
                )
            }
        }
    }
}
