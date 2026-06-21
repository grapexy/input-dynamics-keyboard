// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import android.os.Build
import android.os.SystemClock
import android.system.Os
import android.text.InputType
import android.view.MotionEvent
import androidx.core.content.edit
import helium314.keyboard.keyboard.Key
import helium314.keyboard.keyboard.internal.PopupKeySpec
import helium314.keyboard.keyboard.internal.keyboard_parser.floris.KeyCode
import helium314.keyboard.latin.BuildConfig
import helium314.keyboard.latin.InputAttributes
import helium314.keyboard.latin.common.Constants
import helium314.keyboard.latin.utils.prefs
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.TimeUnit
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong

object ResearchSessionLogger {
    const val PREF_ENABLED = "input_dynamics_logging_enabled"
    private const val PREF_ACTIVE = "input_dynamics_logging_session_active"
    private const val PREF_SESSION_ID = "input_dynamics_logging_session_id"
    private const val PREF_EXTERNAL_RUN_ID = "input_dynamics_logging_external_run_id"
    private const val PREF_INPUT_ACTOR = "input_dynamics_logging_input_actor"
    private const val PREF_INPUT_CONTROLLER = "input_dynamics_logging_input_controller"
    private const val PREF_INPUT_CADENCE_POLICY = "input_dynamics_logging_input_cadence_policy"
    private const val LOG_DIR_NAME = "input_dynamics_logs"
    private const val CONTROL_STATUS_FILE_NAME = "input_dynamics_control_status.json"
    private const val SCHEMA = "input_dynamics_event.v1"
    private const val DEFAULT_INPUT_ACTOR = "human"
    private const val DEFAULT_INPUT_CADENCE_POLICY = "manual"
    private const val CHEAP_RECORD_COUNT_MAX_BYTES = 10L * 1024L * 1024L

    private val ioExecutor = Executors.newSingleThreadExecutor()
    private val pressIdCounter = AtomicLong(0)
    private val pointerPressIds = ConcurrentHashMap<Int, Long>()
    @Volatile private var appContext: Context? = null
    @Volatile private var currentInputAttributes: InputAttributes? = null
    @Volatile private var lifecycleFieldSnapshot: FieldSnapshot? = null
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
        applicationContext.prefs().edit(commit = true) { putBoolean(PREF_ENABLED, enabled) }
    }

    @JvmStatic
    fun isSessionActive(context: Context): Boolean =
        context.prefs().getBoolean(PREF_ACTIVE, false)

    @JvmStatic
    fun currentSessionId(context: Context): String? =
        context.prefs().getString(PREF_SESSION_ID, null)

    @JvmStatic
    fun currentExternalRunId(context: Context): String? =
        context.prefs().getString(PREF_EXTERNAL_RUN_ID, null)

    @JvmStatic
    fun currentInputActor(context: Context): String =
        context.prefs().getString(PREF_INPUT_ACTOR, null)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: DEFAULT_INPUT_ACTOR

    @JvmStatic
    fun currentInputController(context: Context): String? =
        context.prefs().getString(PREF_INPUT_CONTROLLER, null)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }

    @JvmStatic
    fun currentInputCadencePolicy(context: Context): String =
        context.prefs().getString(PREF_INPUT_CADENCE_POLICY, null)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: DEFAULT_INPUT_CADENCE_POLICY

    @JvmStatic
    @JvmOverloads
    fun startSession(
        context: Context,
        externalRunId: String? = null,
        inputActor: String? = null,
        inputController: String? = null,
        inputCadencePolicy: String? = null
    ): String {
        val appContext = rememberContext(context)
        if (isSessionActive(appContext)) {
            stopSession(appContext)
        }
        val sessionId = newSessionId()
        val normalizedExternalRunId = externalRunId?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputActor = inputActor?.trim()?.takeIf { it.isNotEmpty() } ?: DEFAULT_INPUT_ACTOR
        val normalizedInputController = inputController?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputCadencePolicy = inputCadencePolicy
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: DEFAULT_INPUT_CADENCE_POLICY
        pressIdCounter.set(0)
        pointerPressIds.clear()
        appContext.prefs().edit(commit = true) {
            putBoolean(PREF_ACTIVE, true)
            putString(PREF_SESSION_ID, sessionId)
            putString(PREF_INPUT_ACTOR, normalizedInputActor)
            putString(PREF_INPUT_CADENCE_POLICY, normalizedInputCadencePolicy)
            if (normalizedExternalRunId == null) {
                remove(PREF_EXTERNAL_RUN_ID)
            } else {
                putString(PREF_EXTERNAL_RUN_ID, normalizedExternalRunId)
            }
            if (normalizedInputController == null) {
                remove(PREF_INPUT_CONTROLLER)
            } else {
                putString(PREF_INPUT_CONTROLLER, normalizedInputController)
            }
        }
        appendLifecycleEvent(
            appContext,
            SessionSnapshot(
                sessionId,
                normalizedExternalRunId,
                normalizedInputActor,
                normalizedInputController,
                normalizedInputCadencePolicy
            ),
            "session_start"
        )
        return sessionId
    }

    @JvmStatic
    fun stopSession(context: Context): String? {
        val appContext = rememberContext(context)
        if (!isSessionActive(appContext)) return null
        val session = currentSessionSnapshot(appContext) ?: return null
        appendLifecycleEvent(appContext, session, "session_stop")
        appContext.prefs().edit(commit = true) { putBoolean(PREF_ACTIVE, false) }
        lifecycleFieldSnapshot = null
        return session.sessionId
    }

    @JvmStatic
    fun onInputFieldStarted(context: Context, inputAttributes: InputAttributes?) {
        val appContext = rememberContext(context)
        currentInputAttributes = inputAttributes
        knownNonPasswordField = inputAttributes != null && !inputAttributes.mIsPasswordField
        lifecycleFieldSnapshot = if (knownNonPasswordField) {
            FieldSnapshot(inputAttributes?.mTargetApplicationPackageName)
        } else {
            null
        }
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
    fun onInputViewStarted(context: Context, restarting: Boolean) {
        logLifecycleObservation(
            context,
            "input_view_start",
            mapOf("restarting" to restarting)
        )
    }

    @JvmStatic
    fun onInputViewFinished(context: Context, finishingInput: Boolean) {
        logLifecycleObservation(
            context,
            "input_view_finish",
            mapOf("finishing_input" to finishingInput)
        )
    }

    @JvmStatic
    fun onInputFinished(context: Context) {
        logLifecycleObservation(context, "input_finish")
    }

    @JvmStatic
    fun onImeWindowShown(context: Context, inputViewShown: Boolean) {
        logLifecycleObservation(
            context,
            "ime_window_shown",
            mapOf("input_view_shown" to inputViewShown)
        )
    }

    @JvmStatic
    fun onImeWindowHidden(context: Context) {
        logLifecycleObservation(context, "ime_window_hidden")
    }

    @JvmStatic
    fun onImeHideRequest(context: Context, flags: Int) {
        logLifecycleObservation(
            context,
            "ime_hide_request",
            mapOf(
                "flags" to flags,
                "dismissal_source_observed" to "ime_self_hide",
                "dismissal_confidence" to "definitive",
                "dismissal_evidence" to jsonArrayOf("requestHideSelf")
            )
        )
    }

    @JvmStatic
    fun onImeHideWindowCalled(context: Context) {
        logLifecycleObservation(
            context,
            "ime_hide_window_called",
            mapOf(
                "dismissal_source_observed" to "ime_hide_window_called",
                "dismissal_confidence" to "high",
                "dismissal_evidence" to jsonArrayOf("hideWindow")
            )
        )
    }

    @JvmStatic
    fun onSystemBackKeyEvent(
        context: Context,
        keyAction: String,
        keyCode: Int,
        eventTime: Long,
        repeatCount: Int,
        canceled: Boolean
    ) {
        logLifecycleObservation(
            context,
            "system_back_event",
            mapOf(
                "key_action" to keyAction,
                "key_code" to keyCode,
                "t_event_uptime_ms" to eventTime,
                "repeat_count" to repeatCount,
                "canceled" to canceled,
                "dismissal_source_observed" to "system_back",
                "dismissal_confidence" to "high",
                "dismissal_evidence" to jsonArrayOf("key_event")
            )
        )
    }

    @JvmStatic
    fun onEditorAction(context: Context, actionId: Int) {
        logLifecycleObservation(
            context,
            "editor_action",
            mapOf(
                "action_id" to actionId,
                "dismissal_evidence" to jsonArrayOf("performEditorAction")
            )
        )
    }

    @JvmStatic
    fun logMotionEvent(context: Context, event: MotionEvent) {
        val appContext = rememberContext(context)
        if (!canLogInputEvent(appContext)) return
        val session = currentSessionSnapshot(appContext) ?: return
        val actionMasked = event.actionMasked
        val actionName = motionActionName(actionMasked)
        val pointerCount = event.pointerCount
        val actionIndex = event.actionIndex
        updatePressIdsForMotionAction(event, actionMasked, actionIndex, pointerCount)
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
                    historyIndex,
                    pointerPressIds[event.getPointerId(pointerIndex)]
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
                null,
                pointerPressIds[event.getPointerId(pointerIndex)]
            ))
        }

        appendEvents(appContext, session, records, fieldSnapshot())
    }

    @JvmStatic
    fun finishPress(pointerId: Int) {
        pointerPressIds.remove(pointerId)
    }

    @JvmStatic
    fun finishAllPresses() {
        pointerPressIds.clear()
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
        val session = currentSessionSnapshot(appContext) ?: return
        val fields = mutableMapOf<String, Any?>(
            "pointer_id" to pointerId,
            "press_id" to pointerPressIds[pointerId],
            "gesture_id" to pointerPressIds[pointerId],
            "t_event_uptime_ms" to eventTime,
            "x_px" to x,
            "y_px" to y,
            "key_present" to (key != null)
        )
        if (key != null) {
            val code = key.code
            val keyX = key.x
            val keyY = key.y
            val keyWidth = key.width
            val keyHeight = key.height
            val hitBox = key.hitBox
            val popupKeys = key.popupKeys
            fields += mapOf(
                "key_code" to code,
                "key_code_printable" to Constants.printableCode(code),
                "key_label" to key.label,
                "key_hint_label" to key.hintLabel,
                "key_preview_label" to key.previewLabel,
                "key_output_text" to key.outputText,
                "key_icon_name" to key.iconName,
                "key_alt_code" to key.altCode,
                "key_short_string" to key.toShortString(),
                "key_long_string" to key.toLongString(),
                "key_class" to keyClass(key),
                "key_background" to keyBackground(key.backgroundType),
                "key_background_type" to key.backgroundType,
                "key_x_px" to keyX,
                "key_y_px" to keyY,
                "key_width_px" to keyWidth,
                "key_height_px" to keyHeight,
                "key_draw_x_px" to key.drawX,
                "key_draw_width_px" to key.drawWidth,
                "key_horizontal_gap_px" to key.horizontalGap,
                "key_vertical_gap_px" to key.verticalGap,
                "key_hitbox_left_px" to hitBox.left,
                "key_hitbox_top_px" to hitBox.top,
                "key_hitbox_right_px" to hitBox.right,
                "key_hitbox_bottom_px" to hitBox.bottom,
                "key_center_offset_x_px" to (x - (keyX + keyWidth / 2.0)),
                "key_center_offset_y_px" to (y - (keyY + keyHeight / 2.0)),
                "key_touch_x_ratio" to ratio(x - keyX, keyWidth),
                "key_touch_y_ratio" to ratio(y - keyY, keyHeight),
                "key_modifier" to key.isModifier,
                "key_shift" to key.isShift,
                "key_spacer" to key.isSpacer,
                "key_enabled" to key.isEnabled,
                "key_repeatable" to key.isRepeatable,
                "key_preview_enabled" to key.hasPreview(),
                "key_long_press_enabled" to key.isLongPressEnabled,
                "key_alt_code_while_typing" to key.altCodeWhileTyping(),
                "key_has_action_background" to key.hasActionKeyBackground(),
                "key_has_functional_background" to key.hasFunctionalBackground(),
                "key_has_popup_hint" to key.hasPopupHint(),
                "key_has_shifted_letter_hint" to key.hasShiftedLetterHint(),
                "key_has_hint_label" to key.hasHintLabel(),
                "key_has_custom_action_label" to key.hasCustomActionLabel(),
                "key_has_no_panel_auto_popup_key" to key.hasNoPanelAutoPopupKey(),
                "key_has_action_key_popups" to (popupKeys != null && key.hasActionKeyPopups()),
                "key_popup_count" to (popupKeys?.size ?: 0),
                "key_popup_keys_column_number" to key.popupKeysColumnNumber,
                "key_popup_keys_fixed_column" to key.isPopupKeysFixedColumn,
                "key_popup_keys_fixed_order" to key.isPopupKeysFixedOrder,
                "key_popup_keys_have_labels" to key.hasLabelsInPopupKeys(),
                "key_popup_keys_need_dividers" to key.needsDividersInPopupKeys(),
                "key_popup_key_label_flags" to key.popupKeyLabelFlags,
                "key_popup_keys" to popupKeysJson(popupKeys)
            )
        }
        appendEvent(appContext, session, event, fields, includeFieldState = true)
    }

    @JvmStatic
    fun logEvent(context: Context, event: String) {
        logEvent(context, event, emptyMap())
    }

    @JvmStatic
    fun logEvent(context: Context, event: String, fields: Map<String, Any?>) {
        val appContext = rememberContext(context)
        if (!canLogInputEvent(appContext)) return
        val session = currentSessionSnapshot(appContext) ?: return
        appendEvent(appContext, session, event, fields, includeFieldState = true)
    }

    private fun logLifecycleObservation(
        context: Context,
        event: String,
        fields: Map<String, Any?> = emptyMap()
    ) {
        val appContext = rememberContext(context)
        if (!isEnabled(appContext) || !isSessionActive(appContext)) return
        val session = currentSessionSnapshot(appContext) ?: return
        val fieldSnapshot = fieldSnapshot() ?: lifecycleFieldSnapshot ?: return
        appendEvent(appContext, session, event, fields, fieldSnapshot)
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

    fun waitForPendingWrites(timeoutMs: Long = 2_000): Boolean =
        runCatching {
            ioExecutor.submit<Unit> { }.get(timeoutMs, TimeUnit.MILLISECONDS)
            true
        }.getOrDefault(false)

    fun controlStatusJson(
        context: Context,
        requestId: String? = null,
        command: String? = null,
        ok: Boolean = true,
        message: String? = null,
        includeLogs: Boolean = false,
        extraFields: Map<String, Any?> = emptyMap(),
    ): JSONObject {
        val appContext = rememberContext(context)
        val active = isSessionActive(appContext)
        val lastSessionId = currentSessionId(appContext)
        val externalRunId = currentExternalRunId(appContext)
        val logDirectory = logDirectory(appContext)
        val currentLogFile = if (active && lastSessionId != null) {
            File(logDirectory, "session-$lastSessionId.jsonl")
        } else {
            null
        }
        val lastLogFile = currentLogFile ?: listLogFiles(appContext).firstOrNull()
        val packageInfo = appContext.packageManager.getPackageInfo(appContext.packageName, 0)
        val versionCode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            packageInfo.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            packageInfo.versionCode.toLong()
        }
        val statusFile = File(logDirectory, CONTROL_STATUS_FILE_NAME)
        val json = JSONObject()
            .put("package_name", appContext.packageName)
            .put("request_id", jsonValue(requestId))
            .put("version_name", packageInfo.versionName ?: BuildConfig.VERSION_NAME)
            .put("version_code", versionCode)
            .put("build_variant", BuildConfig.BUILD_TYPE)
            .put("debug", BuildConfig.DEBUG)
            .put("enabled", isEnabled(appContext))
            .put("active", active)
            .put("current_session_id", jsonValue(if (active) lastSessionId else null))
            .put("last_session_id", jsonValue(lastSessionId))
            .put("external_run_id", jsonValue(externalRunId))
            .put("input_actor", currentInputActor(appContext))
            .put("input_controller", jsonValue(currentInputController(appContext)))
            .put("input_cadence_policy", currentInputCadencePolicy(appContext))
            .put("log_directory", logDirectory.absolutePath)
            .put("current_log_file_path", jsonValue(currentLogFile?.absolutePath))
            .put("last_log_file_path", jsonValue(lastLogFile?.absolutePath))
            .put("record_count", jsonValue(recordCountIfCheap(lastLogFile)))
            .put("status_file_path", statusFile.absolutePath)
            .put("log_file_count", listLogFiles(appContext).size)
            .put("t_wall_ms", System.currentTimeMillis())
            .put("t_uptime_ms", SystemClock.uptimeMillis())
            .put("ok", ok)
            .put("command", jsonValue(command))
            .put("message", jsonValue(message))

        if (includeLogs) {
            json.put("log_files", logFilesJson(appContext))
        }
        extraFields.forEach { (key, value) ->
            json.put(key, jsonValue(value))
        }
        return json
    }

    @Synchronized
    fun writeControlStatusJson(context: Context, status: JSONObject): File {
        val file = File(logDirectory(context), CONTROL_STATUS_FILE_NAME)
        val tempFile = File(file.parentFile, "$CONTROL_STATUS_FILE_NAME.tmp")
        FileOutputStream(tempFile, false).use { output ->
            output.write(status.toString(2).toByteArray(Charsets.UTF_8))
            output.write('\n'.code)
            output.fd.sync()
        }
        runCatching {
            Os.rename(tempFile.absolutePath, file.absolutePath)
        }.getOrElse {
            if (!tempFile.renameTo(file)) {
                tempFile.copyTo(file, overwrite = true)
                tempFile.delete()
            }
        }
        return file
    }

    private fun appendLifecycleEvent(context: Context, session: SessionSnapshot, event: String) {
        val fields = if (event == "session_start") {
            mapOf(
                "input_actor" to session.inputActor,
                "input_controller" to session.inputController,
                "input_cadence_policy" to session.inputCadencePolicy
            )
        } else {
            emptyMap()
        }
        appendEvent(context, session, event, fields, includeFieldState = false)
    }

    private fun appendEvent(
        context: Context,
        session: SessionSnapshot,
        event: String,
        fields: Map<String, Any?>,
        includeFieldState: Boolean
    ) {
        appendEvents(
            context,
            session,
            listOf(PendingEvent(event, fields)),
            if (includeFieldState) fieldSnapshot() else null
        )
    }

    private fun appendEvent(
        context: Context,
        session: SessionSnapshot,
        event: String,
        fields: Map<String, Any?>,
        fieldSnapshot: FieldSnapshot
    ) {
        appendEvents(
            context,
            session,
            listOf(PendingEvent(event, fields)),
            fieldSnapshot
        )
    }

    private fun appendEvents(
        context: Context,
        session: SessionSnapshot,
        events: List<PendingEvent>,
        fieldSnapshot: FieldSnapshot?
    ) {
        if (events.isEmpty()) return
        val appContext = context.applicationContext
        ioExecutor.execute {
            val target = resolveLogDirectory(appContext)
            val file = File(target.directory, "session-${session.sessionId}.jsonl")
            FileOutputStream(file, true).use { output ->
                events.forEach { event ->
                    val record = JSONObject()
                        .put("schema", SCHEMA)
                        .put("session_id", session.sessionId)
                        .put("external_run_id", jsonValue(session.externalRunId))
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
        historyIndex: Int?,
        pressId: Long?
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
        val touchMajor = if (historyIndex == null) {
            event.getTouchMajor(pointerIndex)
        } else {
            event.getHistoricalTouchMajor(pointerIndex, historyIndex)
        }
        val touchMinor = if (historyIndex == null) {
            event.getTouchMinor(pointerIndex)
        } else {
            event.getHistoricalTouchMinor(pointerIndex, historyIndex)
        }
        val toolMajor = if (historyIndex == null) {
            event.getToolMajor(pointerIndex)
        } else {
            event.getHistoricalToolMajor(pointerIndex, historyIndex)
        }
        val toolMinor = if (historyIndex == null) {
            event.getToolMinor(pointerIndex)
        } else {
            event.getHistoricalToolMinor(pointerIndex, historyIndex)
        }
        val orientation = if (historyIndex == null) {
            event.getOrientation(pointerIndex)
        } else {
            event.getHistoricalOrientation(pointerIndex, historyIndex)
        }
        val fields = mutableMapOf<String, Any?>(
            "sample_kind" to sampleKind,
            "action" to actionMasked,
            "action_name" to actionName,
            "action_index" to actionIndex,
            "device_id" to event.deviceId,
            "source" to event.source,
            "button_state" to event.buttonState,
            "meta_state" to event.metaState,
            "edge_flags" to event.edgeFlags,
            "motion_flags" to event.flags,
            "pointer_count" to event.pointerCount,
            "pointer_id" to event.getPointerId(pointerIndex),
            "press_id" to pressId,
            "gesture_id" to pressId,
            "pointer_index" to pointerIndex,
            "tool_type" to event.getToolType(pointerIndex),
            "t_event_uptime_ms" to eventTime,
            "t_down_uptime_ms" to event.downTime,
            "x_px" to x,
            "y_px" to y,
            "pressure" to pressure,
            "size" to size,
            "touch_major_px" to touchMajor,
            "touch_minor_px" to touchMinor,
            "tool_major_px" to toolMajor,
            "tool_minor_px" to toolMinor,
            "orientation" to orientation
        )
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            fields["classification"] = event.classification
        }
        if (historyIndex != null) {
            fields["history_index"] = historyIndex
        }
        return PendingEvent("pointer_sample", fields)
    }

    private fun updatePressIdsForMotionAction(
        event: MotionEvent,
        actionMasked: Int,
        actionIndex: Int,
        pointerCount: Int
    ) {
        if (actionIndex !in 0 until pointerCount) return
        val pointerId = event.getPointerId(actionIndex)
        when (actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                pointerPressIds.clear()
                beginPress(pointerId)
            }
            MotionEvent.ACTION_POINTER_DOWN -> beginPress(pointerId)
            MotionEvent.ACTION_CANCEL -> {
                if (!pointerPressIds.containsKey(pointerId)) {
                    beginPress(pointerId)
                }
            }
        }
    }

    private fun beginPress(pointerId: Int): Long {
        val pressId = pressIdCounter.incrementAndGet()
        pointerPressIds[pointerId] = pressId
        return pressId
    }

    private fun rememberContext(context: Context): Context {
        val applicationContext = context.applicationContext
        appContext = applicationContext
        return applicationContext
    }

    private fun canLogInputEvent(context: Context): Boolean =
        isEnabled(context) && isSessionActive(context) && knownNonPasswordField

    private fun currentSessionSnapshot(context: Context): SessionSnapshot? {
        val sessionId = currentSessionId(context) ?: return null
        return SessionSnapshot(
            sessionId,
            currentExternalRunId(context),
            currentInputActor(context),
            currentInputController(context),
            currentInputCadencePolicy(context)
        )
    }

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
            is Boolean, is Number, is String, is JSONObject, is JSONArray -> value
            else -> value.toString()
        }

    private fun jsonArrayOf(vararg values: Any?): JSONArray {
        val array = JSONArray()
        values.forEach { value ->
            array.put(jsonValue(value))
        }
        return array
    }

    private fun logFilesJson(context: Context): JSONArray {
        val files = listLogFiles(context)
        val array = JSONArray()
        files.forEach { file ->
            array.put(
                JSONObject()
                    .put("name", file.name)
                    .put("path", file.absolutePath)
                    .put("bytes", file.length())
                    .put("last_modified_ms", file.lastModified())
                    .put("record_count", jsonValue(recordCountIfCheap(file)))
            )
        }
        return array
    }

    private fun recordCountIfCheap(file: File?): Long? {
        if (file == null || !file.exists() || !file.isFile) return null
        if (file.length() > CHEAP_RECORD_COUNT_MAX_BYTES) return null
        var count = 0L
        file.inputStream().buffered().use { input ->
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = input.read(buffer)
                if (read <= 0) break
                for (index in 0 until read) {
                    if (buffer[index] == '\n'.code.toByte()) count++
                }
            }
        }
        return count
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

    private fun popupKeysJson(popupKeys: Array<PopupKeySpec>?): JSONArray? {
        if (popupKeys == null) return null
        val array = JSONArray()
        popupKeys.forEachIndexed { index, popupKey ->
            array.put(
                JSONObject()
                    .put("index", index)
                    .put("code", popupKey.mCode)
                    .put("code_printable", Constants.printableCode(popupKey.mCode))
                    .put("label", jsonValue(popupKey.mLabel))
                    .put("output_text", jsonValue(popupKey.mOutputText))
                    .put("icon_name", jsonValue(popupKey.mIconName))
            )
        }
        return array
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

    private data class SessionSnapshot(
        val sessionId: String,
        val externalRunId: String?,
        val inputActor: String,
        val inputController: String?,
        val inputCadencePolicy: String
    )

    private data class PendingEvent(
        val name: String,
        val fields: Map<String, Any?>
    )

    private data class FieldSnapshot(
        val targetPackage: String?
    )
}
