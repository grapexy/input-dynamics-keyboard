// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import android.os.SystemClock
import android.text.InputType
import android.view.MotionEvent
import androidx.core.content.edit
import helium314.keyboard.keyboard.Key
import helium314.keyboard.keyboard.internal.keyboard_parser.floris.KeyCode
import helium314.keyboard.latin.InputAttributes
import helium314.keyboard.latin.common.Constants
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
    @Volatile private var appContext: Context? = null
    @Volatile private var currentInputAttributes: InputAttributes? = null
    @Volatile private var knownNonPasswordField = false

    @JvmStatic
    fun isEnabled(context: Context): Boolean =
        context.prefs().getBoolean(PREF_ENABLED, false)

    @JvmStatic
    fun setEnabled(context: Context, enabled: Boolean) {
        val applicationContext = rememberContext(context)
        if (!enabled && isSessionActive(applicationContext)) {
            stopSession(applicationContext)
        }
        applicationContext.prefs().edit { putBoolean(PREF_ENABLED, enabled) }
    }

    @JvmStatic
    fun isSessionActive(context: Context): Boolean =
        context.prefs().getBoolean(PREF_ACTIVE, false)

    @JvmStatic
    fun currentSessionId(context: Context): String? =
        context.prefs().getString(PREF_SESSION_ID, null)

    @JvmStatic
    fun startSession(context: Context): String {
        val appContext = rememberContext(context)
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
        val appContext = rememberContext(context)
        val sessionId = currentSessionId(appContext) ?: return null
        appendLifecycleEvent(appContext, sessionId, "session_stop")
        appContext.prefs().edit { putBoolean(PREF_ACTIVE, false) }
        return sessionId
    }

    @JvmStatic
    fun onInputFieldStarted(context: Context, inputAttributes: InputAttributes?) {
        val appContext = rememberContext(context)
        currentInputAttributes = inputAttributes
        knownNonPasswordField = inputAttributes != null && !inputAttributes.mIsPasswordField
        if (!canLogInputEvent(appContext)) return
        val inputType = inputAttributes?.mInputType ?: 0
        val fields = mapOf(
            "input_type" to inputType,
            "input_type_class" to (inputType and InputType.TYPE_MASK_CLASS),
            "input_type_variation" to (inputType and InputType.TYPE_MASK_VARIATION),
            "input_type_flags" to (inputType and InputType.TYPE_MASK_FLAGS)
        )
        logEvent(appContext, "field_enter", fields)
    }

    @JvmStatic
    fun onInputFieldFinished(context: Context) {
        val appContext = rememberContext(context)
        if (canLogInputEvent(appContext)) {
            logEvent(appContext, "field_exit")
        }
        currentInputAttributes = null
        knownNonPasswordField = false
    }

    @JvmStatic
    fun logMotionEvent(context: Context, event: MotionEvent) {
        val appContext = rememberContext(context)
        if (!canLogInputEvent(appContext)) return
        val sessionId = currentSessionId(appContext) ?: return
        val actionMasked = event.actionMasked
        val actionName = motionActionName(actionMasked)
        val pointerCount = event.pointerCount
        val actionIndex = event.actionIndex
        val records = ArrayList<PendingEvent>(pointerCount * (event.historySize + 1))

        for (historyIndex in 0 until event.historySize) {
            val historicalTime = event.getHistoricalEventTime(historyIndex)
            for (pointerIndex in 0 until pointerCount) {
                records.add(pointerSample(
                    event,
                    actionMasked,
                    actionName,
                    actionIndex,
                    pointerIndex,
                    historicalTime,
                    "historical",
                    historyIndex
                ))
            }
        }
        for (pointerIndex in 0 until pointerCount) {
            records.add(pointerSample(
                event,
                actionMasked,
                actionName,
                actionIndex,
                pointerIndex,
                event.eventTime,
                "current",
                null
            ))
        }

        appendEvents(appContext, sessionId, records, includeFieldState = true)
    }

    @JvmStatic
    fun logKeyEvent(
        event: String,
        pointerId: Int,
        x: Int,
        y: Int,
        eventTime: Long,
        key: Key?
    ) {
        val appContext = this.appContext ?: return
        if (!canLogInputEvent(appContext)) return
        val sessionId = currentSessionId(appContext) ?: return
        val fields = mutableMapOf<String, Any?>(
            "pointer_id" to pointerId,
            "t_event_uptime_ms" to eventTime,
            "x_px" to x,
            "y_px" to y,
            "key_present" to (key != null)
        )
        if (key != null) {
            val keyX = key.x
            val keyY = key.y
            val keyWidth = key.width
            val keyHeight = key.height
            fields += mapOf(
                "key_class" to keyClass(key),
                "key_background" to keyBackground(key.backgroundType),
                "key_x_px" to keyX,
                "key_y_px" to keyY,
                "key_width_px" to keyWidth,
                "key_height_px" to keyHeight,
                "key_center_offset_x_px" to (x - (keyX + keyWidth / 2.0)),
                "key_center_offset_y_px" to (y - (keyY + keyHeight / 2.0)),
                "key_touch_x_ratio" to ratio(x - keyX, keyWidth),
                "key_touch_y_ratio" to ratio(y - keyY, keyHeight),
                "key_modifier" to key.isModifier,
                "key_repeatable" to key.isRepeatable
            )
        }
        appendEvent(appContext, sessionId, event, fields, includeFieldState = true)
    }

    @JvmStatic
    fun logEvent(context: Context, event: String) {
        logEvent(context, event, emptyMap())
    }

    @JvmStatic
    fun logEvent(context: Context, event: String, fields: Map<String, Any?>) {
        val appContext = rememberContext(context)
        if (!canLogInputEvent(appContext)) return
        val sessionId = currentSessionId(appContext) ?: return
        appendEvent(appContext, sessionId, event, fields, includeFieldState = true)
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
        appendEvent(context, sessionId, event, emptyMap(), includeFieldState = false)
    }

    private fun appendEvent(
        context: Context,
        sessionId: String,
        event: String,
        fields: Map<String, Any?>,
        includeFieldState: Boolean
    ) {
        appendEvents(
            context,
            sessionId,
            listOf(PendingEvent(event, fields)),
            includeFieldState
        )
    }

    private fun appendEvents(
        context: Context,
        sessionId: String,
        events: List<PendingEvent>,
        includeFieldState: Boolean
    ) {
        if (events.isEmpty()) return
        val appContext = context.applicationContext
        val fieldSnapshot = if (includeFieldState) fieldSnapshot() else null
        ioExecutor.execute {
            val target = resolveLogDirectory(appContext)
            val file = File(target.directory, "session-$sessionId.jsonl")
            FileOutputStream(file, true).use { output ->
                events.forEach { event ->
                    val record = JSONObject()
                        .put("schema", SCHEMA)
                        .put("session_id", sessionId)
                        .put("event", event.name)
                        .put("t_wall_ms", System.currentTimeMillis())
                        .put("t_uptime_ms", SystemClock.uptimeMillis())
                        .put("package_name", appContext.packageName)
                        .put("storage", if (target.external) "app_specific_external" else "internal_fallback")

                    if (fieldSnapshot != null) {
                        record
                            .put("password_field", false)
                            .put("target_package", jsonValue(fieldSnapshot.targetPackage))
                    }

                    event.fields.forEach { (key, value) ->
                        record.put(key, jsonValue(value))
                    }

                    output.write(record.toString().toByteArray(Charsets.UTF_8))
                    output.write('\n'.code)
                }
            }
        }
    }

    private fun pointerSample(
        event: MotionEvent,
        actionMasked: Int,
        actionName: String,
        actionIndex: Int,
        pointerIndex: Int,
        eventTime: Long,
        sampleKind: String,
        historyIndex: Int?
    ): PendingEvent {
        val x = if (historyIndex == null) {
            event.getX(pointerIndex)
        } else {
            event.getHistoricalX(pointerIndex, historyIndex)
        }
        val y = if (historyIndex == null) {
            event.getY(pointerIndex)
        } else {
            event.getHistoricalY(pointerIndex, historyIndex)
        }
        val pressure = if (historyIndex == null) {
            event.getPressure(pointerIndex)
        } else {
            event.getHistoricalPressure(pointerIndex, historyIndex)
        }
        val size = if (historyIndex == null) {
            event.getSize(pointerIndex)
        } else {
            event.getHistoricalSize(pointerIndex, historyIndex)
        }
        val fields = mutableMapOf<String, Any?>(
            "sample_kind" to sampleKind,
            "action" to actionMasked,
            "action_name" to actionName,
            "action_index" to actionIndex,
            "pointer_count" to event.pointerCount,
            "pointer_id" to event.getPointerId(pointerIndex),
            "pointer_index" to pointerIndex,
            "t_event_uptime_ms" to eventTime,
            "t_down_uptime_ms" to event.downTime,
            "x_px" to x,
            "y_px" to y,
            "pressure" to pressure,
            "size" to size
        )
        if (historyIndex != null) {
            fields["history_index"] = historyIndex
        }
        return PendingEvent("pointer_sample", fields)
    }

    private fun rememberContext(context: Context): Context {
        val applicationContext = context.applicationContext
        appContext = applicationContext
        return applicationContext
    }

    private fun canLogInputEvent(context: Context): Boolean =
        isEnabled(context) && isSessionActive(context) && knownNonPasswordField

    private fun fieldSnapshot(): FieldSnapshot? {
        val inputAttributes = currentInputAttributes ?: return null
        if (!knownNonPasswordField) return null
        return FieldSnapshot(inputAttributes.mTargetApplicationPackageName)
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

    private fun motionActionName(action: Int): String =
        when (action) {
            MotionEvent.ACTION_DOWN -> "down"
            MotionEvent.ACTION_UP -> "up"
            MotionEvent.ACTION_MOVE -> "move"
            MotionEvent.ACTION_CANCEL -> "cancel"
            MotionEvent.ACTION_POINTER_DOWN -> "pointer_down"
            MotionEvent.ACTION_POINTER_UP -> "pointer_up"
            MotionEvent.ACTION_HOVER_MOVE -> "hover_move"
            MotionEvent.ACTION_SCROLL -> "scroll"
            MotionEvent.ACTION_HOVER_ENTER -> "hover_enter"
            MotionEvent.ACTION_HOVER_EXIT -> "hover_exit"
            else -> "other"
        }

    private fun keyClass(key: Key): String {
        val code = key.code
        return when {
            code == Constants.CODE_SPACE || code == KeyCode.CJK_SPACE || code == KeyCode.ZWNJ -> "space"
            code == Constants.CODE_ENTER || code == KeyCode.SHIFT_ENTER -> "enter"
            code == Constants.CODE_TAB || code == KeyCode.TAB -> "tab"
            code == KeyCode.DELETE -> "delete"
            code == KeyCode.LANGUAGE_SWITCH -> "language_switch"
            code == KeyCode.EMOJI || code == KeyCode.EMOJI_SEARCH -> "emoji"
            code == KeyCode.CLIPBOARD ||
                    code == KeyCode.CLIPBOARD_COPY ||
                    code == KeyCode.CLIPBOARD_CUT ||
                    code == KeyCode.CLIPBOARD_PASTE ||
                    code == KeyCode.CLIPBOARD_SELECT_WORD ||
                    code == KeyCode.CLIPBOARD_SELECT_ALL ||
                    code == KeyCode.CLIPBOARD_CLEAR_HISTORY ||
                    code == KeyCode.CLIPBOARD_COPY_ALL -> "clipboard"
            key.isModifier -> "modifier"
            key.hasActionKeyBackground() -> "action"
            code < 0 -> "function"
            Character.isLetter(code) -> "letter"
            Character.isDigit(code) -> "digit"
            Character.isWhitespace(code) -> "whitespace"
            code >= Constants.CODE_SPACE -> "symbol"
            else -> "unknown"
        }
    }

    private fun keyBackground(backgroundType: Int): String =
        when (backgroundType) {
            Key.BACKGROUND_TYPE_EMPTY -> "empty"
            Key.BACKGROUND_TYPE_NORMAL -> "normal"
            Key.BACKGROUND_TYPE_FUNCTIONAL -> "functional"
            Key.BACKGROUND_TYPE_ACTION -> "action"
            Key.BACKGROUND_TYPE_SPACEBAR -> "spacebar"
            else -> "unknown"
        }

    private fun ratio(numerator: Number, denominator: Int): Double? =
        if (denominator == 0) null else numerator.toDouble() / denominator.toDouble()

    private fun newSessionId(): String {
        val formatter = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US)
        formatter.timeZone = TimeZone.getTimeZone("UTC")
        return formatter.format(Date()) + "-" + UUID.randomUUID().toString().take(8)
    }

    private data class LogDirectory(
        val directory: File,
        val external: Boolean
    )

    private data class PendingEvent(
        val name: String,
        val fields: Map<String, Any?>
    )

    private data class FieldSnapshot(
        val targetPackage: String?
    )
}
