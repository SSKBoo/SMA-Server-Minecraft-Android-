package com.bedrockdroid.rs.model

import android.content.Context
import android.content.Intent
import android.os.Process
import android.widget.Toast
import androidx.core.content.FileProvider
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.io.BufferedReader
import java.io.File
import java.io.FileOutputStream
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.text.SimpleDateFormat
import java.util.ArrayDeque
import java.util.Date
import java.util.Locale
import java.util.concurrent.atomic.AtomicLong

object LogBuffer {
    const val DEFAULT_MAX_LINES = 500
    const val LOGCAT_STREAM_FILE_NAME = "logcat_pipe.log"

    private val lock = Any()
    private val streamFileLock = Any()
    private val seq = AtomicLong(10L)
    private var maxLines: Int = DEFAULT_MAX_LINES
    private val ring = ArrayDeque<ConsoleLogLine>(DEFAULT_MAX_LINES)

    @Volatile
    private var appContext: Context? = null

    private val collectorScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var logcatJob: Job? = null
    @Volatile
    private var logcatProcess: java.lang.Process? = null

    private val _logs = MutableStateFlow<List<ConsoleLogLine>>(emptyList())
    val logs: StateFlow<List<ConsoleLogLine>> = _logs.asStateFlow()

    init {
        try {
            append(LogStream.DEBUG, "[LogBuffer] Buffer UI acotado (capacidad=$DEFAULT_MAX_LINES líneas); LogcatPipe en streaming MODE_APPEND.", "LogBuffer")
            append(LogStream.STDOUT, "BedrockDroid RS · Núcleo Rust ARM64 (JNI) listo.", "System")
            append(LogStream.STDOUT, "Toca 'Iniciar', usa un chip rápido o escribe 'help'.", "System")
        } catch (_: Throwable) {
            // Nunca abortar inicialización estática del objeto
        }
    }

    fun initContext(context: Context) {
        appContext = context.applicationContext
    }

    fun getLogcatStreamFile(context: Context): File =
        File(context.filesDir, LOGCAT_STREAM_FILE_NAME)

    fun configureMaxLines(limit: Int) {
        synchronized(lock) {
            maxLines = limit.coerceIn(100, 1000)
            while (ring.size > maxLines) {
                ring.removeFirst()
            }
            _logs.value = ring.toList()
        }
    }

    fun append(stream: LogStream, text: String, tag: String = "BedrockDroidRS") {
        try {
            val line = ConsoleLogLine(
                id = seq.incrementAndGet(),
                time = nowStamp(),
                stream = stream,
                text = text,
                tag = tag
            )
            synchronized(lock) {
                if (ring.size >= maxLines) {
                    ring.removeFirst()
                }
                ring.addLast(line)
                _logs.value = ring.toList()
            }
        } catch (_: OutOfMemoryError) {
            runCatching { System.gc() }
            synchronized(lock) {
                ring.clear()
                _logs.value = emptyList()
            }
        }
    }

    fun appendLevel(level: LogLevel, tag: String, message: String) {
        val stream = when (level) {
            LogLevel.DEBUG -> LogStream.DEBUG
            LogLevel.INFO -> LogStream.STDOUT
            LogLevel.WARN -> LogStream.WARN
            LogLevel.ERROR -> LogStream.ERR
        }
        append(stream = stream, text = "[$tag] $message", tag = tag)
    }

    fun clear() {
        synchronized(lock) {
            ring.clear()
            val resetLine = ConsoleLogLine(
                id = seq.incrementAndGet(),
                time = nowStamp(),
                stream = LogStream.STDOUT,
                text = "Consola limpia. Escribe 'help' para ver comandos.",
                tag = "LogBuffer"
            )
            ring.addLast(resetLine)
            _logs.value = ring.toList()
        }
    }

    fun clearStreamLogFile(context: Context) {
        val ctx = context.applicationContext
        synchronized(streamFileLock) {
            runCatching {
                ctx.openFileOutput(LOGCAT_STREAM_FILE_NAME, Context.MODE_PRIVATE).use { fos ->
                    fos.write(ByteArray(0))
                    fos.flush()
                    fos.close()
                }
            }
            val file = getLogcatStreamFile(ctx)
            if (file.exists()) {
                runCatching {
                    FileOutputStream(file, false).use { fos ->
                        fos.write(ByteArray(0))
                        fos.flush()
                        fos.close()
                    }
                    file.delete()
                }
            }
        }
    }

    fun snapshotText(entries: List<ConsoleLogLine> = _logs.value): String {
        return entries.joinToString("\n") { line ->
            "[${line.time}] [${line.level.badge}] [${line.tag}] ${line.text}"
        }
    }

    fun startProcessLogcatCollector(context: Context? = null) {
        try {
            if (context != null) {
                appContext = context.applicationContext
            }
            if (logcatJob?.isActive == true) return
            val ctx = (context ?: appContext)?.applicationContext
            val pid = Process.myPid()
            val command = arrayOf(
                "logcat", "-v", "brief", "--pid=$pid", "-s",
                "BedrockDroidRS:V", "bedrockdroid.rs:V", "RustJNI:V", "LogBuffer:V", "BedrockDroidUI:V",
                "*:S" // Silencia todo lo demás
            )
            appendLevel(
                LogLevel.DEBUG,
                "LogcatPipe",
                "Iniciando captura logcat en streaming MODE_APPEND -> $LOGCAT_STREAM_FILE_NAME (PID=$pid)"
            )

            logcatJob = collectorScope.launch {
                var streamFos: FileOutputStream? = null
                var streamWriter: OutputStreamWriter? = null
                try {
                    if (ctx != null) {
                        streamFos = ctx.openFileOutput(LOGCAT_STREAM_FILE_NAME, Context.MODE_APPEND)
                        streamWriter = OutputStreamWriter(streamFos, Charsets.UTF_8)
                    }
                    val proc = ProcessBuilder(*command)
                        .redirectErrorStream(true)
                        .start()
                    logcatProcess = proc
                    BufferedReader(InputStreamReader(proc.inputStream)).use { reader ->
                        while (isActive) {
                            val raw = reader.readLine() ?: break
                            val formatted = formatStreamLogcatLine(raw) ?: continue
                            if (streamWriter != null) {
                                synchronized(streamFileLock) {
                                    streamWriter.write(formatted)
                                    streamWriter.write("\n")
                                    streamWriter.flush()
                                }
                            }
                        }
                    }
                } catch (_: OutOfMemoryError) {
                    runCatching { System.gc() }
                    runCatching {
                        streamWriter?.write("[OOM] OutOfMemoryError en LogcatPipe\n")
                        streamWriter?.flush()
                    }
                } catch (_: SecurityException) {
                    if (isActive) {
                        appendLevel(
                            LogLevel.WARN,
                            "LogcatPipe",
                            "logcat bloqueado por SELinux en este dispositivo; usando logs internos + JNI."
                        )
                    }
                } catch (err: Throwable) {
                    if (isActive) {
                        appendLevel(
                            LogLevel.WARN,
                            "LogcatPipe",
                            "Captura logcat no disponible (${err.javaClass.simpleName}): ${err.message}"
                        )
                    }
                } finally {
                    runCatching { streamWriter?.flush() }
                    runCatching { streamWriter?.close() }
                    runCatching { streamFos?.close() }
                }
            }
        } catch (err: Throwable) {
            appendLevel(LogLevel.WARN, "LogcatPipe", "No se pudo iniciar collector logcat: ${err.message}")
        }
    }

    fun stopProcessLogcatCollector() {
        try {
            logcatJob?.cancel()
            logcatJob = null
            logcatProcess?.destroy()
            logcatProcess = null
        } catch (_: Throwable) {
            logcatProcess = null
        }
    }

    @Volatile
    var verboseDiagnostics: Boolean = false

    fun setVerboseDiagnostics(enabled: Boolean): Boolean {
        verboseDiagnostics = enabled
        appendLevel(
            LogLevel.INFO,
            "LogcatPipe",
            if (enabled) "Modo diagnóstico --verbose activado (sin filtro INT32_MAX)." else "Modo normal activo (filtro anti-spam INT32_MAX habilitado)."
        )
        return enabled
    }

    private fun formatStreamLogcatLine(raw: String): String? {
        val trimmed = raw.trim()
        if (trimmed.isBlank() || trimmed.startsWith("--------- beginning of")) return null
        if (!verboseDiagnostics && trimmed.contains("Note: end time exceeds INT32_MAX")) return null
        val level = when {
            trimmed.startsWith("E/") || trimmed.startsWith("F/") || trimmed.contains(" E/") || trimmed.contains(" F/") || trimmed.contains(" E ") || trimmed.contains(" F ") -> LogLevel.ERROR
            trimmed.startsWith("W/") || trimmed.contains(" W/") || trimmed.contains(" W ") -> LogLevel.WARN
            trimmed.startsWith("I/") || trimmed.contains(" I/") || trimmed.contains(" I ") -> LogLevel.INFO
            trimmed.startsWith("D/") || trimmed.startsWith("V/") || trimmed.contains(" D/") || trimmed.contains(" V/") || trimmed.contains(" D ") || trimmed.contains(" V ") -> LogLevel.DEBUG
            else -> return null
        }
        val tag = when {
            trimmed.contains("BedrockDroidRS") -> "BedrockDroidRS"
            trimmed.contains("bedrockdroid.rs") -> "bedrockdroid.rs"
            trimmed.contains("RustJNI") -> "RustJNI"
            trimmed.contains("LogBuffer") -> "LogBuffer"
            trimmed.contains("BedrockDroidUI") -> "BedrockDroidUI"
            else -> return null
        }
        return "[${nowStamp()}] [${level.badge}] [$tag] $trimmed"
    }

    fun exportLogs(context: Context): File? {
        return runCatching {
            val logsDir = File(context.filesDir, "logs").apply {
                if (!exists()) mkdirs()
            }
            val stamp = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(Date())
            val outFile = File(logsDir, "bedrockdroid_${stamp}.log")
            val header = buildString {
                appendLine("=== BedrockDroid RS Diagnostic Log ===")
                appendLine("Timestamp: ${SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US).format(Date())}")
                appendLine("PID: ${Process.myPid()}")
                appendLine("Lines: ${_logs.value.size}")
                appendLine("======================================")
            }
            FileOutputStream(outFile, false).use { outFos ->
                outFos.write((header + snapshotText() + "\n").toByteArray(Charsets.UTF_8))
                val streamFile = getLogcatStreamFile(context)
                if (streamFile.exists() && streamFile.length() > 0L) {
                    outFos.write("\n=== LogcatPipe Stream ===\n".toByteArray(Charsets.UTF_8))
                    streamFile.inputStream().use { inStream ->
                        val buffer = ByteArray(8192)
                        while (true) {
                            val read = inStream.read(buffer)
                            if (read <= 0) break
                            outFos.write(buffer, 0, read)
                        }
                    }
                }
                outFos.flush()
            }

            runCatching {
                Toast.makeText(
                    context,
                    "Log guardado en: ${outFile.absolutePath}",
                    Toast.LENGTH_LONG
                ).show()
            }

            shareFileWithChooser(
                context = context,
                file = outFile,
                subject = "BedrockDroid RS Logs ($stamp)",
                chooserTitle = "Compartir logs de BedrockDroid RS"
            )
            outFile
        }.onFailure { err ->
            appendLevel(LogLevel.ERROR, "ExportLogs", "Error al exportar logs: ${err.message}")
            runCatching {
                Toast.makeText(
                    context,
                    "No se pudo exportar el log: ${err.message}",
                    Toast.LENGTH_LONG
                ).show()
            }
        }.getOrNull()
    }

    fun readLatestCrashReport(context: Context): File? {
        return try {
            val crashDir = File(context.filesDir, "crash")
            if (!crashDir.exists() || !crashDir.isDirectory) return null
            crashDir.listFiles { f -> f.isFile && f.name.startsWith("crash_") && f.name.endsWith(".txt") }
                ?.maxByOrNull { it.lastModified() }
        } catch (_: Throwable) {
            null
        }
    }

    fun clearCrashReports(context: Context) {
        try {
            val crashDir = File(context.filesDir, "crash")
            crashDir.listFiles()?.forEach { runCatching { it.delete() } }
        } catch (_: Throwable) {
            // Ignorar error de limpieza
        }
    }

    fun shareFileWithChooser(
        context: Context,
        file: File,
        subject: String,
        chooserTitle: String
    ) {
        val uri = FileProvider.getUriForFile(
            context,
            "${context.packageName}.fileprovider",
            file
        )
        val shareIntent = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_SUBJECT, subject)
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        val chooser = Intent.createChooser(shareIntent, chooserTitle).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(chooser)
    }

    private fun nowStamp(): String {
        return try {
            SimpleDateFormat("HH:mm:ss", Locale.US).format(Date())
        } catch (_: Throwable) {
            "00:00:00"
        }
    }
}
