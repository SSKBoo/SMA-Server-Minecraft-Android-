package com.bedrockdroid.rs.crash

import android.content.Context
import android.util.Log
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.ServerRuntimeState
import com.bedrockdroid.rs.nativebridge.RustCoreBridge
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

object CrashHandler {
    private const val PREFS_NAME = "bedrockdroid_rs_prefs"
    private const val KEY_PROTECTION_TEST_MODE = "crash_protection_test_mode"

    @Volatile
    private var appContext: Context? = null

    private val _protectionEnabled = MutableStateFlow(false)
    val protectionEnabled: StateFlow<Boolean> = _protectionEnabled.asStateFlow()

    private val _hasPendingCrash = MutableStateFlow(false)
    val hasPendingCrash: StateFlow<Boolean> = _hasPendingCrash.asStateFlow()

    private val _crashLogContent = MutableStateFlow("")
    val crashLogContent: StateFlow<String> = _crashLogContent.asStateFlow()

    @JvmStatic
    fun init(context: Context) {
        val appCtx = context.applicationContext
        appContext = appCtx
        val enabled = appCtx
            .getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .getBoolean(KEY_PROTECTION_TEST_MODE, false)
        _protectionEnabled.value = enabled
        RustCoreBridge.setCrashProtectionEnabledSafe(enabled)
        refreshState(appCtx)
    }

    @JvmStatic
    fun isProtectionEnabled(context: Context? = appContext): Boolean {
        val ctx = context ?: appContext
        if (ctx != null && appContext == null) {
            return ctx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .getBoolean(KEY_PROTECTION_TEST_MODE, false)
        }
        return _protectionEnabled.value
    }

    @JvmStatic
    fun setProtectionEnabled(context: Context, enabled: Boolean) {
        val appCtx = context.applicationContext
        appContext = appCtx
        appCtx.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(KEY_PROTECTION_TEST_MODE, enabled)
            .apply()
        _protectionEnabled.value = enabled
        RustCoreBridge.setCrashProtectionEnabledSafe(enabled)
    }

    @JvmStatic
    fun refreshState(context: Context? = appContext) {
        val ctx = context ?: appContext ?: return
        try {
            val content = CrashLogFileManager.readCrashLogPreview(ctx)
            _crashLogContent.value = content
            _hasPendingCrash.value = content.isNotBlank() || CrashLogFileManager.hasPendingCrash(ctx)
        } catch (oom: OutOfMemoryError) {
            logCriticalThrowable(ctx, "CrashHandler.refreshState", oom)
        } catch (_: Throwable) {
            // Ignorar error de lectura secundaria
        }
    }

    @JvmStatic
    fun logCrash(error: String) {
        if (!_protectionEnabled.value) {
            return
        }
        Log.e("BedrockDroidCrash", error)
        val ctx = appContext
        if (ctx != null) {
            runCatching { CrashLogFileManager.appendCrash(ctx, error) }
            refreshState(ctx)
        } else {
            _hasPendingCrash.value = true
            _crashLogContent.value = (_crashLogContent.value + "\n" + error).trimStart()
        }
        runCatching {
            ServerRuntimeState.appendLog(LogStream.ERR, "[CRASH] ${error.take(1500)}", "CrashHandler")
        }
    }

    @JvmStatic
    fun logCriticalThrowable(context: Context? = appContext, origin: String, throwable: Throwable) {
        val ctx = (context ?: appContext)?.applicationContext
        if (throwable is OutOfMemoryError) {
            runCatching { System.gc() }
        }
        val summary = try {
            if (throwable is OutOfMemoryError) {
                "OutOfMemoryError en $origin: ${throwable.message ?: "heap agotado"}"
            } else {
                "Throwable en $origin (${throwable.javaClass.name}): ${throwable.message}\n${throwable.stackTraceToString().take(4096)}"
            }
        } catch (_: Throwable) {
            "CriticalThrowable en $origin: ${throwable.javaClass.simpleName}"
        }
        runCatching { Log.e("BedrockDroidCrash", summary) }
        if (ctx != null && (throwable is OutOfMemoryError || isProtectionEnabled(ctx))) {
            runCatching { CrashLogFileManager.appendCrash(ctx, summary) }
            runCatching {
                _crashLogContent.value = CrashLogFileManager.readCrashLogPreview(ctx)
                _hasPendingCrash.value = true
            }
        }
    }

    @JvmStatic
    fun clearLogs(context: Context) {
        CrashLogFileManager.clearCrashLog(context)
        runCatching { LogBuffer.clearStreamLogFile(context) }
        _crashLogContent.value = ""
        _hasPendingCrash.value = false
        refreshState(context)
    }
}
