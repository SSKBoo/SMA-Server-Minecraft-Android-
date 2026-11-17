package com.bedrockdroid.rs

import android.app.Application
import com.bedrockdroid.rs.crash.CrashHandler
import com.bedrockdroid.rs.crash.CrashLogFileManager
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.nativebridge.RustCoreBridge
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

class BedrockDroidApp : Application() {
    override fun onCreate() {
        LogBuffer.initContext(this)
        CrashHandler.init(this)
        RustCoreBridge.registerCrashCallbackSafe(
            CrashLogFileManager.getCrashLogFile(this).absolutePath,
            CrashHandler.isProtectionEnabled(this)
        ) { error ->
            CrashHandler.logCrash(error)
        }
        installCrashHandler()
        super.onCreate()
    }

    private fun installCrashHandler() {
        val defaultHandler = Thread.getDefaultUncaughtExceptionHandler()
        val crashDir = File(filesDir, "crash")
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            if (throwable is OutOfMemoryError || CrashHandler.isProtectionEnabled(this)) {
                try {
                    if (throwable is OutOfMemoryError) {
                        CrashHandler.logCriticalThrowable(this, "Uncaught:${thread.name}", throwable)
                    } else {
                        val reportText = buildString {
                            appendLine("Thread: ${thread.name}")
                            appendLine("Time: ${Date()}")
                            appendLine("Exception: ${throwable.javaClass.name}")
                            appendLine("Message: ${throwable.message}")
                            appendLine("--- Stacktrace ---")
                            appendLine(throwable.stackTraceToString())
                            appendLine("--- Cause ---")
                            var cause = throwable.cause
                            while (cause != null) {
                                appendLine(cause.stackTraceToString())
                                cause = cause.cause
                            }
                        }
                        try {
                            crashDir.mkdirs()
                            val ts = SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(Date())
                            val file = File(crashDir, "crash_${ts}.txt")
                            file.writeText(reportText)
                        } catch (_: Throwable) {
                            // Ignorar error secundario en carpeta interna
                        }
                        CrashHandler.logCrash(reportText)
                    }
                } catch (_: Throwable) {
                    // Si incluso el crash handler falla, no podemos hacer nada
                }
            }
            // Delegar al handler por defecto para que el sistema muestre el crash
            defaultHandler?.uncaughtException(thread, throwable)
        }
    }
}
