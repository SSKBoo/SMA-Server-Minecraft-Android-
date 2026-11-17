package com.bedrockdroid.rs.engine

import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.net.TrafficStats
import android.os.Build
import android.os.Process
import android.os.StatFs
import android.os.Trace
import androidx.core.content.ContextCompat
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.EngineLane
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.JavaFlavor
import com.bedrockdroid.rs.model.LauncherPrefs
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.NetworkProbeReport
import com.bedrockdroid.rs.model.NetworkStateKind
import com.bedrockdroid.rs.model.ServerRuntimeState
import com.bedrockdroid.rs.nativebridge.RustCoreBridge
import com.bedrockdroid.rs.service.ServerForegroundService
import org.json.JSONObject
import java.io.File
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.HttpURLConnection
import java.net.Inet4Address
import java.net.InetAddress
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

    fun probeNetwork(cfg: AndroidLauncherConfig, allowSsdp: Boolean = true): NetworkProbeReport {
        ServerRuntimeState.setProbingNetwork(true)
        Trace.beginSection("nethernet.probe")
        return try {
            val raw = RustCoreBridge.probeNetworkAndUpnpSafe(
                cfg.bedrockPort,
                cfg.javaPort,
                cfg.enableUpnp && allowSsdp,
                cfg.dualStackIpv6
            ) ?: "{}"
            val obj = runCatching { JSONObject(raw) }.getOrNull()
            val upnp = obj?.optString("upnp", "no_igd_or_cgnat") ?: "no_igd_or_cgnat"
            val igdWanIp = obj?.optString("igd_wan_ip", "")?.takeIf { it.isNotBlank() && it != "null" }
            val realDualStack = obj?.optBoolean("dual_stack_v6", cfg.dualStackIpv6) ?: cfg.dualStackIpv6
            val realV6Only = obj?.optBoolean("ipv6_v6only", false) ?: false
            val realBindV4 = obj?.optString("bind_v4", "0.0.0.0") ?: "0.0.0.0"
            val realBindV6 = obj?.optString("bind_v6", "none") ?: "none"

            val lanIp = findLocalLanIpv4() ?: "127.0.0.1"
            val publicIp = fetchPublicIpv4()
            val port = if (cfg.activeLane == EngineLane.JAVA) cfg.javaPort else cfg.bedrockPort

            val igdIsPrivateOrCgnat = igdWanIp != null && isPrivateOrCgnatIpv4(igdWanIp)
            val wanMismatch = igdWanIp != null && publicIp != null && igdWanIp != publicIp

            val networkState = when {
                lanIp == "127.0.0.1" -> NetworkStateKind.CGNAT
                upnp == "igd_gateway_found" && (igdIsPrivateOrCgnat || wanMismatch) -> NetworkStateKind.CGNAT
                upnp == "igd_gateway_found" -> NetworkStateKind.UPNP_OK
                else -> NetworkStateKind.LOCAL_ONLY
            }

            val isCgnat = networkState == NetworkStateKind.CGNAT
            val guidance = when (networkState) {
                NetworkStateKind.UPNP_OK ->
                    "En este móvil juega por 127.0.0.1:$port (<1 ms). Wi-Fi LAN: $lanIp:$port y router UPnP listo${if (publicIp != null) " (WAN $publicIp)" else ""}."
                NetworkStateKind.CGNAT ->
                    "CGNAT detectado${if (publicIp != null) " (WAN=$publicIp, IGD=${igdWanIp ?: "móvil"})" else ""}. Port-forward no funcionará. Usa túnel (playit.gg / Tailscale / ngrok)."
                NetworkStateKind.LOCAL_ONLY ->
                    "Router sin UPnP IGD: abre $port/tcp y $port/udp manualmente. LAN $lanIp:$port lista."
            }

            val report = NetworkProbeReport(
                lanIp = lanIp,
                loopbackHint = "127.0.0.1:$port",
                upnpStatus = upnp,
                dualStackV6 = realDualStack,
                isLikelyCgnat = isCgnat,
                guidance = guidance,
                networkState = networkState,
                publicIp = publicIp,
                igdWanIp = igdWanIp,
                bindV4 = realBindV4,
                bindV6 = realBindV6,
                ipv6V6Only = realV6Only
            )
            ServerRuntimeState.updateNetworkReport(report)
            report
        } finally {
            Trace.endSection()
        }
    }

    private fun fetchPublicIpv4(): String? {
        return runCatching {
            val conn = (URL("https://api.ipify.org").openConnection() as HttpURLConnection).apply {
                requestMethod = "GET"
                connectTimeout = 2000
                readTimeout = 2000
                setRequestProperty("User-Agent", "BedrockDroidRS/1.0")
            }
            val body = conn.inputStream.bufferedReader().readText().trim()
            conn.disconnect()
            body.takeIf { it.matches(Regex("""^\d{1,3}(\.\d{1,3}){3}$""")) }
        }.getOrNull()
    }

    private fun isPrivateOrCgnatIpv4(ip: String): Boolean {
        val parts = ip.split(".").mapNotNull { it.toIntOrNull() }
        if (parts.size != 4) return false
        val (a, b) = parts[0] to parts[1]
        return a == 10 ||
            (a == 172 && b in 16..31) ||
            (a == 192 && b == 168) ||
            (a == 100 && b in 64..127)
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
                val ifaceName = iface.name.lowercase()
                if (ifaceName.startsWith("tun") || ifaceName.startsWith("ppp") || ifaceName.startsWith("rmnet")) continue
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
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  ping | test                      -> Prueba señalización HTTP NetherNet (/v1/join) en 127.0.0.1")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  pinglan                          -> Ping UDP Kotlin (sin JNI) a 127.0.0.1, LAN y ::1")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  sockets | ss | netstat           -> Inspecciona sockets UDP/UID sin JNI")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  netcheck | upnp status           -> Prueba LAN, WAN (api.ipify.org), UPnP y CGNAT")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  verbose on | off | --verbose     -> Alterna filtro INT32_MAX en LogcatPipe")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  versions                         -> Lista rutas en /files/engines")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  convert hybrid                   -> Crea perfil Híbrido aparte")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  soc                              -> Lee topología ARM64 vía JNI")
                ServerRuntimeState.appendLog(LogStream.STDOUT, "  clear                            -> Limpia la consola")
            }
            lower == "verbose on" || lower == "--verbose" -> {
                LogBuffer.setVerboseDiagnostics(true)
            }
            lower == "verbose off" -> {
                LogBuffer.setVerboseDiagnostics(false)
            }
            lower == "ping" || lower == "test" -> {
                Trace.beginSection("nethernet.join")
                val pingResponse = try {
                    RustCoreBridge.sendPingCommandSafe(cfg.bedrockPort)
                } finally {
                    Trace.endSection()
                }
                val stream = if (pingResponse.contains("responde", ignoreCase = true)) {
                    LogStream.NET
                } else {
                    LogStream.WARN
                }
                ServerRuntimeState.appendLog(stream, pingResponse, "RustPing")
            }
            lower == "pinglan" -> {
                inspectUdpTargetsFromKotlin(cfg.bedrockPort)
            }
            lower == "sockets" || lower == "ss" || lower == "netstat" -> {
                inspectAndroidUdpSockets(cfg.bedrockPort)
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
                    "[Net] Estado=${probe.networkState.id} · Loopback=${probe.loopbackHint} · LAN=${probe.lanIp} · WAN=${probe.publicIp ?: "n/a"} · UPnP=${probe.upnpStatus}"
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

    private fun inspectAndroidUdpSockets(bedrockPort: Int) {
        val port = if (bedrockPort <= 0) 19132 else bedrockPort
        val hexPort = port.toString(16).uppercase().padStart(4, '0')
        val uid = Process.myUid()
        val rxPackets = TrafficStats.getUidRxPackets(uid)
        val rxBytes = TrafficStats.getUidRxBytes(uid)
        val txPackets = TrafficStats.getUidTxPackets(uid)
        val txBytes = TrafficStats.getUidTxBytes(uid)
        ServerRuntimeState.appendLog(
            LogStream.NET,
            "[Sockets UID=$uid] TrafficStats Rx=${rxPackets} pkts (${rxBytes} B) · Tx=${txPackets} pkts (${txBytes} B)"
        )

        var procBlockedBySelinux = false
        listOf("/proc/self/net/udp", "/proc/self/net/udp6").forEach { path ->
            runCatching {
                val lines = File(path).readLines()
                val matches = lines.filter { it.contains(":$hexPort ", ignoreCase = true) }
                if (matches.isEmpty()) {
                    ServerRuntimeState.appendLog(LogStream.STDOUT, "[$path] Sin entradas para puerto :$port (0x$hexPort)")
                } else {
                    matches.forEach { line ->
                        ServerRuntimeState.appendLog(LogStream.NET, "[$path] ${line.trim()}")
                    }
                }
            }.onFailure {
                procBlockedBySelinux = true
            }
        }

        if (procBlockedBySelinux) {
            ServerRuntimeState.appendLog(
                LogStream.STDOUT,
                "[Sockets] Diagnóstico de sockets limitado por Android (SELinux). Comportamiento normal a partir de API 29."
            )
        }

        runCatching {
            val snmp = File("/proc/self/net/snmp").readLines()
            snmp.filter { it.startsWith("Udp:") }.forEach { line ->
                ServerRuntimeState.appendLog(LogStream.STDOUT, "[/proc/self/net/snmp] $line")
            }
        }

        listOf(arrayOf("ss", "-anu"), arrayOf("netstat", "-anu")).forEach { cmd ->
            if (procBlockedBySelinux) {
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[${cmd[0]}] no disponible (restringido por SELinux en API 29+)"
                )
                return@forEach
            }
            runCatching {
                val proc = ProcessBuilder(*cmd).redirectErrorStream(true).start()
                val out = proc.inputStream.bufferedReader().readLines()
                val matchingRows = out.filter { it.contains(":$port") }
                if (matchingRows.isNotEmpty()) {
                    matchingRows.take(10).forEach { line ->
                        ServerRuntimeState.appendLog(LogStream.NET, "[${cmd[0]}] $line")
                    }
                } else {
                    ServerRuntimeState.appendLog(
                        LogStream.STDOUT,
                        "[${cmd[0]}] no disponible para :$port"
                    )
                }
            }.onFailure {
                ServerRuntimeState.appendLog(
                    LogStream.STDOUT,
                    "[${cmd[0]}] no disponible"
                )
            }
        }
    }

    private fun inspectUdpTargetsFromKotlin(bedrockPort: Int) {
        val port = if (bedrockPort <= 0) 19132 else bedrockPort
        val lanIp = findLocalLanIpv4()
        val targets = buildList {
            add("127.0.0.1")
            if (lanIp != null && lanIp != "127.0.0.1") add(lanIp)
            add("::1")
        }
        val magic = byteArrayOf(
            0x00, 0xFF.toByte(), 0xFF.toByte(), 0x00,
            0xFE.toByte(), 0xFE.toByte(), 0xFE.toByte(), 0xFE.toByte(),
            0xFD.toByte(), 0xFD.toByte(), 0xFD.toByte(), 0xFD.toByte(),
            0x12, 0x34, 0x56, 0x78
        )
        val ping = ByteArray(33).apply {
            this[0] = 0x01
            val ts = System.currentTimeMillis()
            for (i in 0 until 8) this[1 + i] = ((ts ushr (56 - i * 8)) and 0xFF).toByte()
            System.arraycopy(magic, 0, this, 9, 16)
            for (i in 0 until 8) this[25 + i] = ((0x4B4F544C494E01L ushr (56 - i * 8)) and 0xFF).toByte()
        }

        targets.forEach { host ->
            runCatching {
                DatagramSocket().use { sock ->
                    sock.soTimeout = 1500
                    val addr = InetAddress.getByName(host)
                    val startNs = System.nanoTime()
                    sock.send(DatagramPacket(ping, ping.size, addr, port))
                    val recvBuf = ByteArray(2048)
                    val resp = DatagramPacket(recvBuf, recvBuf.size)
                    sock.receive(resp)
                    val ms = (System.nanoTime() - startNs) / 1_000_000.0
                    val motdLen = if (resp.length > 35) ((recvBuf[33].toInt() and 0xFF) shl 8) or (recvBuf[34].toInt() and 0xFF) else 0
                    val end = (35 + motdLen).coerceAtMost(resp.length)
                    val motd = if (end > 35) String(recvBuf, 35, end - 35, Charsets.UTF_8) else "id=0x${(recvBuf[0].toInt() and 0xFF).toString(16)}"
                    ServerRuntimeState.appendLog(
                        LogStream.NET,
                        "[PingKotlin -> $host:$port] OK en ${"%.2f".format(ms)} ms · $motd"
                    )
                }
            }.onFailure { err ->
                ServerRuntimeState.appendLog(
                    LogStream.WARN,
                    "[PingKotlin -> $host:$port] Sin respuesta (${err.javaClass.simpleName}: ${err.message})"
                )
            }
        }
    }
}
