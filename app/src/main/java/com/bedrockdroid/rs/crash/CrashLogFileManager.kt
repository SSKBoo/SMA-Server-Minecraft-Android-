package com.bedrockdroid.rs.crash

import android.content.Context
import android.net.Uri
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStreamWriter
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

object CrashLogFileManager {
    const val CRASH_LOG_FILE_NAME = "crash_log.txt"

    @JvmStatic
    fun getCrashLogFile(context: Context): File {
        val baseDir = context.getExternalFilesDir(null) ?: context.filesDir
        return File(baseDir, CRASH_LOG_FILE_NAME)
    }

    @JvmStatic
    @Synchronized
    fun appendCrash(context: Context, error: String) {
        val file = getCrashLogFile(context)
        file.parentFile?.mkdirs()
        val ts = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS", Locale.US).format(Date())
        val entry = buildString {
            appendLine("=== [$ts] CRASH / RUST PANIC ===")
            appendLine(error.trimEnd())
            appendLine()
        }
        FileOutputStream(file, true).use { fos ->
            OutputStreamWriter(fos, Charsets.UTF_8).use { writer ->
                writer.write(entry)
                writer.flush()
                runCatching { fos.fd.sync() }
                writer.close()
            }
            fos.close()
        }
    }

    @JvmStatic
    @Synchronized
    fun readCrashLog(context: Context): String {
        val file = getCrashLogFile(context)
        if (!file.exists()) return ""
        return runCatching { file.readText(Charsets.UTF_8) }.getOrDefault("")
    }

    @JvmStatic
    fun hasPendingCrash(context: Context): Boolean {
        val file = getCrashLogFile(context)
        return file.exists() && file.length() > 0L
    }

    @JvmStatic
    @Synchronized
    fun clearCrashLog(context: Context) {
        val file = getCrashLogFile(context)
        if (file.exists()) {
            runCatching {
                FileOutputStream(file, false).use { fos ->
                    fos.flush()
                    fos.close()
                }
                file.delete()
            }
        }
    }

    @JvmStatic
    fun exportCrashLogToUri(context: Context, uri: Uri): Result<Long> = runCatching {
        val content = readCrashLog(context).ifBlank {
            "No hay registros en crash_log.txt (${getCrashLogFile(context).absolutePath})."
        }
        val bytes = content.toByteArray(Charsets.UTF_8)
        val outStream = context.contentResolver.openOutputStream(uri)
            ?: error("No se pudo abrir flujo de salida para el documento seleccionado")
        outStream.use { os ->
            os.write(bytes)
            os.flush()
            os.close()
        }
        bytes.size.toLong()
    }
}
