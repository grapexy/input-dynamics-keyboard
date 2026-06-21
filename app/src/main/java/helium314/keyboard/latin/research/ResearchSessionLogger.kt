// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import android.os.SystemClock
import androidx.core.content.edit
import helium314.keyboard.latin.utils.prefs
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.UUID
import java.util.concurrent.Executors

object ResearchSessionLogger {
    const val PREF_ENABLED = "research_logging_enabled"
    private const val PREF_ACTIVE = "research_logging_session_active"
    private const val PREF_SESSION_ID = "research_logging_session_id"
    private const val LOG_DIR_NAME = "research_typing_logs"
    private const val SCHEMA = "typing_event.v1"

    private val ioExecutor = Executors.newSingleThreadExecutor()

    @JvmStatic
    fun isEnabled(context: Context): Boolean =
        context.prefs().getBoolean(PREF_ENABLED, false)

    @JvmStatic
    fun setEnabled(context: Context, enabled: Boolean) {
        if (!enabled && isSessionActive(context)) {
            stopSession(context)
        }
        context.prefs().edit { putBoolean(PREF_ENABLED, enabled) }
    }

    @JvmStatic
    fun isSessionActive(context: Context): Boolean =
        context.prefs().getBoolean(PREF_ACTIVE, false)

    @JvmStatic
    fun currentSessionId(context: Context): String? =
        context.prefs().getString(PREF_SESSION_ID, null)

    @JvmStatic
    fun startSession(context: Context): String {
        val appContext = context.applicationContext
        val sessionId = newSessionId()
        appContext.prefs().edit {
            putBoolean(PREF_ACTIVE, true)
            putString(PREF_SESSION_ID, sessionId)
        }
        appendLifecycleEvent(appContext, sessionId, "session_start")
        return sessionId
    }

    @JvmStatic
    fun stopSession(context: Context): String? {
        val appContext = context.applicationContext
        val sessionId = currentSessionId(appContext) ?: return null
        appendLifecycleEvent(appContext, sessionId, "session_stop")
        appContext.prefs().edit { putBoolean(PREF_ACTIVE, false) }
        return sessionId
    }

    @JvmStatic
    fun logEvent(context: Context, event: String) {
        logEvent(context, event, emptyMap())
    }

    @JvmStatic
    fun logEvent(context: Context, event: String, fields: Map<String, Any?>) {
        val appContext = context.applicationContext
        if (!isEnabled(appContext) || !isSessionActive(appContext)) return
        val sessionId = currentSessionId(appContext) ?: return
        appendEvent(appContext, sessionId, event, fields)
    }

    fun logDirectory(context: Context): File =
        resolveLogDirectory(context.applicationContext).directory

    fun adbPullCommand(context: Context): String =
        "adb pull ${logDirectory(context).absolutePath}/ ."

    fun listLogFiles(context: Context): List<File> =
        logDirectory(context).listFiles { file ->
            file.isFile && file.name.endsWith(".jsonl")
        }?.sortedByDescending { it.lastModified() }.orEmpty()

    fun deleteAllLogs(context: Context): Int {
        var deleted = 0
        listLogFiles(context).forEach {
            if (it.delete()) deleted++
        }
        return deleted
    }

    private fun appendLifecycleEvent(context: Context, sessionId: String, event: String) {
        appendEvent(context, sessionId, event, emptyMap())
    }

    private fun appendEvent(
        context: Context,
        sessionId: String,
        event: String,
        fields: Map<String, Any?>
    ) {
        val appContext = context.applicationContext
        ioExecutor.execute {
            val target = resolveLogDirectory(appContext)
            val record = JSONObject()
                .put("schema", SCHEMA)
                .put("session_id", sessionId)
                .put("event", event)
                .put("t_wall_ms", System.currentTimeMillis())
                .put("t_uptime_ms", SystemClock.uptimeMillis())
                .put("package_name", appContext.packageName)
                .put("storage", if (target.external) "app_specific_external" else "internal_fallback")

            fields.forEach { (key, value) ->
                record.put(key, jsonValue(value))
            }

            val file = File(target.directory, "session-$sessionId.jsonl")
            FileOutputStream(file, true).use {
                it.write(record.toString().toByteArray(Charsets.UTF_8))
                it.write('\n'.code)
            }
        }
    }

    private fun resolveLogDirectory(context: Context): LogDirectory {
        val external = context.getExternalFilesDir(LOG_DIR_NAME)
        val directory = external ?: File(context.filesDir, LOG_DIR_NAME)
        directory.mkdirs()
        return LogDirectory(directory, external != null)
    }

    private fun jsonValue(value: Any?): Any =
        when (value) {
            null -> JSONObject.NULL
            is Boolean, is Number, is String -> value
            else -> value.toString()
        }

    private fun newSessionId(): String {
        val formatter = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US)
        formatter.timeZone = TimeZone.getTimeZone("UTC")
        return formatter.format(Date()) + "-" + UUID.randomUUID().toString().take(8)
    }

    private data class LogDirectory(
        val directory: File,
        val external: Boolean
    )
}
