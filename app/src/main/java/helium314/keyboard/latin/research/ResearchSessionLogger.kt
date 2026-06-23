// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.os.SystemClock
import android.system.Os
import android.text.InputType
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import androidx.core.content.edit
import helium314.keyboard.compat.EditorInfoCompatUtils
import helium314.keyboard.keyboard.Key
import helium314.keyboard.keyboard.Keyboard
import helium314.keyboard.keyboard.KeyboardView
import helium314.keyboard.keyboard.internal.PopupKeySpec
import helium314.keyboard.keyboard.internal.keyboard_parser.floris.KeyCode
import helium314.keyboard.latin.BuildConfig
import helium314.keyboard.latin.InputAttributes
import helium314.keyboard.latin.common.Constants
import helium314.keyboard.latin.utils.InputTypeUtils
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
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sqrt

object ResearchSessionLogger {
    const val PREF_ENABLED = "input_dynamics_logging_enabled"
    private const val PREF_ACTIVE = "input_dynamics_logging_session_active"
    private const val PREF_SESSION_ID = "input_dynamics_logging_session_id"
    private const val PREF_EXTERNAL_RUN_ID = "input_dynamics_logging_external_run_id"
    private const val PREF_INPUT_ACTOR = "input_dynamics_logging_input_actor"
    private const val PREF_INPUT_CONTROLLER = "input_dynamics_logging_input_controller"
    private const val PREF_INPUT_CADENCE_POLICY = "input_dynamics_logging_input_cadence_policy"
    private const val PREF_INPUT_PROFILE_SOURCE = "input_dynamics_logging_input_profile_source"
    private const val PREF_INPUT_PROFILE_ID = "input_dynamics_logging_input_profile_id"
    private const val PREF_INPUT_PROFILE_SCHEMA = "input_dynamics_logging_input_profile_schema"
    private const val PREF_INPUT_PROFILE_HASH = "input_dynamics_logging_input_profile_hash"
    private const val PREF_INPUT_PROFILE_SEED = "input_dynamics_logging_input_profile_seed"
    private const val LOG_DIR_NAME = "input_dynamics_logs"
    private const val CONTROL_STATUS_FILE_NAME = "input_dynamics_control_status.json"
    private const val CONTROL_RESULT_FILE_PREFIX = "input_dynamics_control_result_"
    private const val CONTROL_RESULT_FILE_SUFFIX = ".json"
    private const val SCHEMA = "input_dynamics_event.v1"
    private const val DEFAULT_INPUT_ACTOR = "human"
    private const val DEFAULT_INPUT_CADENCE_POLICY = "manual"
    private const val CHEAP_RECORD_COUNT_MAX_BYTES = 10L * 1024L * 1024L
    private const val FIELD_EPISODE_REUSE_WINDOW_MS = 1_500L
    private const val MIN_ACTIVE_KEY_NEAR_THRESHOLD_PX = 8.0
    private const val CLOCK_DOMAIN_ANDROID_UPTIME_MS = "android_uptime_ms"
    private const val CLOCK_DOMAIN_DEVICE_ELAPSED_REALTIME_NS = "device_elapsed_realtime_ns"
    private const val TIMESTAMP_PRECISION_MILLISECONDS = "milliseconds"
    private const val TIMESTAMP_PRECISION_NANOSECONDS = "nanoseconds"
    private const val TIMESTAMP_PRECISION_MILLISECONDS_CONVERTED_TO_NANOSECONDS =
        "milliseconds_converted_to_nanoseconds"
    private const val TIMESTAMP_SOURCE_MOTION_EVENT = "motion_event"
    private const val TIMESTAMP_SOURCE_KEY_EVENT = "key_event"
    private const val TIMESTAMP_SOURCE_CALLBACK_CAPTURE = "callback_capture"
    private const val TIMESTAMP_SOURCE_SYNTHETIC_HANDLER = "synthetic_handler"
    private const val TIMESTAMP_SOURCE_WRITER = "writer"

    private val ioExecutor = Executors.newSingleThreadExecutor()
    private val pressIdCounter = AtomicLong(0)
    private val fieldEpisodeCounter = AtomicLong(0)
    private val pointerPressIds = ConcurrentHashMap<Int, Long>()
    @Volatile private var appContext: Context? = null
    @Volatile private var currentInputAttributes: InputAttributes? = null
    @Volatile private var lifecycleFieldSnapshot: FieldSnapshot? = null
    @Volatile private var currentFieldEpisodeId: Long? = null
    @Volatile private var currentFieldSignature: String? = null
    @Volatile private var lastFinishedFieldEpisodeId: Long? = null
    @Volatile private var lastFinishedFieldSignature: String? = null
    @Volatile private var lastFinishedFieldUptimeMs: Long = 0L
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
    fun currentInputProfileSource(context: Context): String? =
        currentOptionalString(context, PREF_INPUT_PROFILE_SOURCE)

    @JvmStatic
    fun currentInputProfileId(context: Context): String? =
        currentOptionalString(context, PREF_INPUT_PROFILE_ID)

    @JvmStatic
    fun currentInputProfileSchema(context: Context): String? =
        currentOptionalString(context, PREF_INPUT_PROFILE_SCHEMA)

    @JvmStatic
    fun currentInputProfileHash(context: Context): String? =
        currentOptionalString(context, PREF_INPUT_PROFILE_HASH)

    @JvmStatic
    fun currentInputProfileSeed(context: Context): String? =
        currentOptionalString(context, PREF_INPUT_PROFILE_SEED)

    @JvmStatic
    @JvmOverloads
    fun startSession(
        context: Context,
        externalRunId: String? = null,
        inputActor: String? = null,
        inputController: String? = null,
        inputCadencePolicy: String? = null,
        inputProfileSource: String? = null,
        inputProfileId: String? = null,
        inputProfileSchema: String? = null,
        inputProfileHash: String? = null,
        inputProfileSeed: String? = null
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
        val normalizedInputProfileSource = inputProfileSource?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputProfileId = inputProfileId?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputProfileSchema = inputProfileSchema?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputProfileHash = inputProfileHash?.trim()?.takeIf { it.isNotEmpty() }
        val normalizedInputProfileSeed = inputProfileSeed?.trim()?.takeIf { it.isNotEmpty() }
        pressIdCounter.set(0)
        fieldEpisodeCounter.set(0)
        pointerPressIds.clear()
        resetFieldEpisodeIds()
        lifecycleFieldSnapshot = currentInputAttributes
            ?.takeIf { knownNonPasswordField }
            ?.let { beginFieldEpisode(it) }
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
            putOptionalString(PREF_INPUT_PROFILE_SOURCE, normalizedInputProfileSource)
            putOptionalString(PREF_INPUT_PROFILE_ID, normalizedInputProfileId)
            putOptionalString(PREF_INPUT_PROFILE_SCHEMA, normalizedInputProfileSchema)
            putOptionalString(PREF_INPUT_PROFILE_HASH, normalizedInputProfileHash)
            putOptionalString(PREF_INPUT_PROFILE_SEED, normalizedInputProfileSeed)
        }
        appendLifecycleEvent(
            appContext,
            SessionSnapshot(
                sessionId,
                normalizedExternalRunId,
                normalizedInputActor,
                normalizedInputController,
                normalizedInputCadencePolicy,
                normalizedInputProfileSource,
                normalizedInputProfileId,
                normalizedInputProfileSchema,
                normalizedInputProfileHash,
                normalizedInputProfileSeed
            ),
            "session_start"
        )
        val currentFieldAttributes = currentInputAttributes
        val currentFieldSnapshot = lifecycleFieldSnapshot
        if (currentFieldAttributes != null && knownNonPasswordField && currentFieldSnapshot != null) {
            logFieldEnter(
                appContext,
                currentFieldAttributes,
                currentFieldSnapshot,
                "session_start_current_field"
            )
        }
        return sessionId
    }

    @JvmStatic
    fun stopSession(context: Context): String? {
        val appContext = rememberContext(context)
        if (!isSessionActive(appContext)) return null
        val session = currentSessionSnapshot(appContext) ?: return null
        appendLifecycleEvent(appContext, session, "session_stop")
        appContext.prefs().edit(commit = true) { putBoolean(PREF_ACTIVE, false) }
        resetFieldEpisodeIds()
        return session.sessionId
    }

    @JvmStatic
    fun onInputFieldStarted(context: Context, inputAttributes: InputAttributes?) {
        val appContext = rememberContext(context)
        val nonPasswordAttributes = inputAttributes?.takeUnless { it.mIsPasswordField }
        if (nonPasswordAttributes == null) {
            clearFieldEpisodeState(clearLastFinished = true)
            return
        }
        currentInputAttributes = nonPasswordAttributes
        knownNonPasswordField = true
        lifecycleFieldSnapshot = beginFieldEpisode(nonPasswordAttributes)
        if (!canLogInputEvent(appContext)) return
        logFieldEnter(appContext, nonPasswordAttributes, fieldSnapshot())
    }

    @JvmStatic
    fun onInputFieldFinished(context: Context) {
        val appContext = rememberContext(context)
        if (canLogInputEvent(appContext)) {
            logEvent(appContext, "field_exit")
        }
        rememberFinishedFieldEpisode()
        currentInputAttributes = null
        knownNonPasswordField = false
        currentFieldEpisodeId = null
        currentFieldSignature = null
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
            ),
            eventTime = eventTimeMetadata(TIMESTAMP_SOURCE_KEY_EVENT)
        )
    }

    @JvmStatic
    fun onEditorAction(context: Context, actionId: Int) {
        logLifecycleObservation(
            context,
            "editor_action",
            mapOf(
                "action_id" to actionId,
                "action_name" to editorActionName(actionId),
                "dismissal_evidence" to jsonArrayOf("performEditorAction")
            ) + editorInfoFields(currentInputAttributes, includeFieldActionId = false)
        )
    }

    @JvmStatic
    fun logMotionEvent(context: Context, event: MotionEvent) {
        logMotionEvent(context, event, null)
    }

    @JvmStatic
    fun logMotionEvent(context: Context, event: MotionEvent, keyboardView: View?) {
        val appContext = rememberContext(context)
        if (!canLogInputEvent(appContext)) return
        val session = currentSessionSnapshot(appContext) ?: return
        val actionMasked = event.actionMasked
        val actionName = motionActionName(actionMasked)
        val pointerCount = event.pointerCount
        val actionIndex = event.actionIndex
        val coordinateFrame = ResearchCoordinateFrameSnapshot.fromView(context, keyboardView)
        val keyboard = (keyboardView as? KeyboardView)?.keyboard
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
                    pointerPressIds[event.getPointerId(pointerIndex)],
                    coordinateFrame,
                    keyboard
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
                pointerPressIds[event.getPointerId(pointerIndex)],
                coordinateFrame,
                keyboard
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
        val coordinateFrame = ResearchCoordinateFrameSnapshot.current(appContext)
        val fields = mutableMapOf<String, Any?>(
            "pointer_id" to pointerId,
            "press_id" to pointerPressIds[pointerId],
            "gesture_id" to pointerPressIds[pointerId],
            "t_event_uptime_ms" to eventTime,
            "x_px" to x,
            "y_px" to y,
            "key_present" to (key != null)
        )
        fields += coordinateFrame.fieldsForLocalPoint(x, y)
        fields += ResearchKeyboardLayoutSnapshot.currentStateFields(appContext)
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
        appendEvent(
            appContext,
            session,
            event,
            fields,
            includeFieldState = true,
            eventTime = keyEventTimeMetadata(event)
        )
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
        appendEvent(
            appContext,
            session,
            event,
            ResearchKeyboardLayoutSnapshot.currentStateFields(appContext) + fields,
            includeFieldState = true
        )
    }

    private fun logLifecycleObservation(
        context: Context,
        event: String,
        fields: Map<String, Any?> = emptyMap(),
        eventTime: TimestampMetadata? = null
    ) {
        val appContext = rememberContext(context)
        if (!isEnabled(appContext) || !isSessionActive(appContext)) return
        val session = currentSessionSnapshot(appContext) ?: return
        val fieldSnapshot = fieldSnapshot() ?: lifecycleFieldSnapshot ?: return
        appendEvent(
            appContext,
            session,
            event,
            ResearchKeyboardLayoutSnapshot.currentStateFields(appContext) +
                    ResearchCoordinateFrameSnapshot.current(appContext).fields() +
                    fields,
            fieldSnapshot,
            eventTime = eventTime
        )
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
        val lastExternalRunId = currentExternalRunId(appContext)
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
        val resultFile = requestId?.let {
            File(logDirectory, controlResultFileName(it))
        }
        val inputScope = inputScopeStatus(appContext, active)
        val uptimeMs = SystemClock.uptimeMillis()
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
            .put("external_run_id", jsonValue(if (active) lastExternalRunId else null))
            .put("last_external_run_id", jsonValue(lastExternalRunId))
            .put("input_actor", currentInputActor(appContext))
            .put("input_controller", jsonValue(currentInputController(appContext)))
            .put("input_cadence_policy", currentInputCadencePolicy(appContext))
            .put("input_profile_source", jsonValue(currentInputProfileSource(appContext)))
            .put("input_profile_id", jsonValue(currentInputProfileId(appContext)))
            .put("input_profile_schema", jsonValue(currentInputProfileSchema(appContext)))
            .put("input_profile_hash", jsonValue(currentInputProfileHash(appContext)))
            .put("input_profile_seed", jsonValue(currentInputProfileSeed(appContext)))
            .put("input_scope_ready", inputScope.ready)
            .put("input_scope_state", inputScope.state)
            .put("current_target_package", jsonValue(inputScope.fieldSnapshot?.targetPackage))
            .put("current_field_episode_id", jsonValue(inputScope.fieldSnapshot?.fieldEpisodeId))
            .put("log_directory", logDirectory.absolutePath)
            .put("current_log_file_path", jsonValue(currentLogFile?.absolutePath))
            .put("last_log_file_path", jsonValue(lastLogFile?.absolutePath))
            .put("record_count", jsonValue(recordCountIfCheap(lastLogFile)))
            .put("status_file_path", statusFile.absolutePath)
            .put("result_file_path", jsonValue(resultFile?.absolutePath))
            .put("log_file_count", listLogFiles(appContext).size)
            .put("t_wall_ms", System.currentTimeMillis())
            .put("t_uptime_ms", uptimeMs)
            .put("t_uptime_ns", TimeUnit.MILLISECONDS.toNanos(uptimeMs))
            .put("t_elapsed_realtime_ns", SystemClock.elapsedRealtimeNanos())
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
        writeJsonFile(file, CONTROL_STATUS_FILE_NAME, status)
        return file
    }

    @Synchronized
    fun writeControlResultJson(context: Context, requestId: String?, status: JSONObject): File? {
        val normalizedRequestId = requestId?.trim()?.takeIf { it.isNotEmpty() } ?: return null
        val fileName = controlResultFileName(normalizedRequestId)
        val file = File(logDirectory(context), fileName)
        writeJsonFile(file, fileName, status)
        return file
    }

    private fun writeJsonFile(file: File, tempNamePrefix: String, status: JSONObject) {
        val tempFile = File(file.parentFile, "$tempNamePrefix.tmp")
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
    }

    private fun controlResultFileName(requestId: String): String {
        val safeRequestId = requestId.map { char ->
            if (
                char in 'a'..'z' ||
                char in 'A'..'Z' ||
                char in '0'..'9' ||
                char == '-' ||
                char == '_'
            ) {
                char
            } else {
                '_'
            }
        }.joinToString("")
        return "$CONTROL_RESULT_FILE_PREFIX$safeRequestId$CONTROL_RESULT_FILE_SUFFIX"
    }

    private fun appendLifecycleEvent(context: Context, session: SessionSnapshot, event: String) {
        val fields = if (event == "session_start") {
            mapOf(
                "input_actor" to session.inputActor,
                "input_controller" to session.inputController,
                "input_cadence_policy" to session.inputCadencePolicy,
                "input_profile_source" to session.inputProfileSource,
                "input_profile_id" to session.inputProfileId,
                "input_profile_schema" to session.inputProfileSchema,
                "input_profile_hash" to session.inputProfileHash,
                "input_profile_seed" to session.inputProfileSeed
            )
        } else {
            emptyMap()
        }
        appendEvent(context, session, event, fields, includeFieldState = false)
    }

    private fun logFieldEnter(
        context: Context,
        inputAttributes: InputAttributes,
        fieldSnapshot: FieldSnapshot?,
        source: String? = null
    ) {
        val snapshot = fieldSnapshot ?: return
        val sourceField = source?.let { mapOf("field_enter_source" to it) }.orEmpty()
        val fields = ResearchKeyboardLayoutSnapshot.currentStateFields(context) +
                inputFieldFields(inputAttributes) +
                sourceField
        val session = currentSessionSnapshot(context) ?: return
        appendEvent(context, session, "field_enter", fields, snapshot)
    }

    private fun appendEvent(
        context: Context,
        session: SessionSnapshot,
        event: String,
        fields: Map<String, Any?>,
        includeFieldState: Boolean,
        eventTime: TimestampMetadata? = null,
        downTime: TimestampMetadata? = null
    ) {
        appendEvents(
            context,
            session,
            listOf(PendingEvent(event, fields, eventTime, downTime)),
            if (includeFieldState) fieldSnapshot() else null
        )
    }

    private fun appendEvent(
        context: Context,
        session: SessionSnapshot,
        event: String,
        fields: Map<String, Any?>,
        fieldSnapshot: FieldSnapshot,
        eventTime: TimestampMetadata? = null,
        downTime: TimestampMetadata? = null
    ) {
        appendEvents(
            context,
            session,
            listOf(PendingEvent(event, fields, eventTime, downTime)),
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
        val captureTimestamp = CaptureTimestamp.now()
        val capturedEvents = events.map { CapturedPendingEvent(it, captureTimestamp) }
        ioExecutor.execute {
            val target = resolveLogDirectory(appContext)
            val file = File(target.directory, "session-${session.sessionId}.jsonl")
            FileOutputStream(file, true).use { output ->
                capturedEvents.forEach { capturedEvent ->
                    val event = capturedEvent.event
                    val uptimeMs = SystemClock.uptimeMillis()
                    val elapsedRealtimeNs = SystemClock.elapsedRealtimeNanos()
                    val record = JSONObject()
                        .put("schema", SCHEMA)
                        .put("session_id", session.sessionId)
                        .put("external_run_id", jsonValue(session.externalRunId))
                        .put("event", event.name)
                        .put("t_wall_ms", System.currentTimeMillis())
                        .put("t_uptime_ms", uptimeMs)
                        .put("t_uptime_ns", TimeUnit.MILLISECONDS.toNanos(uptimeMs))
                        .put("t_elapsed_realtime_ns", elapsedRealtimeNs)
                        .put("package_name", appContext.packageName)
                        .put("storage", if (target.external) "app_specific_external" else "internal_fallback")

                    if (fieldSnapshot != null) {
                        record
                            .put("password_field", false)
                            .put("target_package", jsonValue(fieldSnapshot.targetPackage))
                            .put("field_episode_id", fieldSnapshot.fieldEpisodeId)
                    }

                    event.fields.forEach { (key, value) ->
                        record.put(key, jsonValue(value))
                    }
                    addNanosecondCompanion(record, "t_event_uptime_ms", "t_event_uptime_ns")
                    addNanosecondCompanion(record, "t_down_uptime_ms", "t_down_uptime_ns")
                    record
                        .put("t_capture_elapsed_realtime_ns", capturedEvent.captureTimestamp.elapsedRealtimeNs)
                        .put("capture_time", captureTimeMetadata().toJson())
                        .put("write_time", writeTimeMetadata().toJson())
                    event.eventTime?.let { record.put("event_time", it.toJson()) }
                    event.downTime?.let { record.put("down_time", it.toJson()) }

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
        pressId: Long?,
        coordinateFrame: ResearchCoordinateFrameSnapshot.CoordinateFrameSnapshot,
        keyboard: Keyboard?
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
        val toolType = event.getToolType(pointerIndex)
        val fields = mutableMapOf<String, Any?>(
            "sample_kind" to sampleKind,
            "action" to actionMasked,
            "action_name" to actionName,
            "motion_action" to actionMasked,
            "motion_action_name" to actionName,
            "action_index" to actionIndex,
            "motion_action_index" to actionIndex,
            "device_id" to event.deviceId,
            "source" to event.source,
            "input_device_id" to event.deviceId,
            "motion_source" to event.source,
            "button_state" to event.buttonState,
            "meta_state" to event.metaState,
            "edge_flags" to event.edgeFlags,
            "motion_flags" to event.flags,
            "pointer_count" to event.pointerCount,
            "pointer_id" to event.getPointerId(pointerIndex),
            "press_id" to pressId,
            "gesture_id" to pressId,
            "pointer_index" to pointerIndex,
            "tool_type" to toolType,
            "tool_type_name" to toolTypeName(toolType),
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
        fields += coordinateFrame.fieldsForLocalPoint(x, y)
        fields += ResearchKeyboardLayoutSnapshot.stateFieldsForKeyboard(keyboard)
        fields += activeKeyContextFields(keyboard, x, y)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            fields["classification"] = event.classification
            fields["classification_name"] = motionClassificationName(event.classification)
        }
        if (historyIndex != null) {
            fields["history_index"] = historyIndex
        }
        return PendingEvent(
            "pointer_sample",
            fields,
            eventTime = eventTimeMetadata(TIMESTAMP_SOURCE_MOTION_EVENT),
            downTime = downTimeMetadata(TIMESTAMP_SOURCE_MOTION_EVENT)
        )
    }

    private fun activeKeyContextFields(keyboard: Keyboard?, x: Float, y: Float): Map<String, Any?> {
        if (keyboard == null) {
            return mapOf(
                "active_key_present" to false,
                "active_key_lookup" to "keyboard_unavailable"
            )
        }

        val localX = x.roundToInt()
        val localY = y.roundToInt()
        val hitKey = detectHitKey(keyboard, localX, localY)
        val activeKey = hitKey ?: nearestKey(keyboard, localX, localY)
        if (activeKey == null) {
            return mapOf(
                "active_key_present" to false,
                "active_key_lookup" to "no_nearby_key"
            )
        }

        val keyX = activeKey.x
        val keyY = activeKey.y
        val keyWidth = activeKey.width
        val keyHeight = activeKey.height
        val hitBox = activeKey.hitBox
        val nearThresholdPx = activeKeyNearThresholdPx(activeKey)
        val distanceToBoundsPx = distanceToBoundsPx(
            x.toDouble(),
            y.toDouble(),
            keyX,
            keyY,
            keyWidth,
            keyHeight
        )
        val relation = activeKeyRelationForSample(
            x = x.toDouble(),
            y = y.toDouble(),
            keyX = keyX,
            keyY = keyY,
            keyWidth = keyWidth,
            keyHeight = keyHeight,
            hitBoxLeft = hitBox.left,
            hitBoxTop = hitBox.top,
            hitBoxRight = hitBox.right,
            hitBoxBottom = hitBox.bottom,
            nearThresholdPx = nearThresholdPx
        )

        return mapOf(
            "active_key_present" to true,
            "active_key_lookup" to if (hitKey != null) "hitbox" else "nearest",
            "active_key_relation" to relation,
            "active_key_code" to activeKey.code,
            "active_key_code_printable" to Constants.printableCode(activeKey.code),
            "active_key_label" to activeKey.label,
            "active_key_output_text" to activeKey.outputText,
            "active_key_class" to keyClass(activeKey),
            "active_key_x_px" to keyX,
            "active_key_y_px" to keyY,
            "active_key_width_px" to keyWidth,
            "active_key_height_px" to keyHeight,
            "active_key_hitbox_left_px" to hitBox.left,
            "active_key_hitbox_top_px" to hitBox.top,
            "active_key_hitbox_right_px" to hitBox.right,
            "active_key_hitbox_bottom_px" to hitBox.bottom,
            "active_key_center_offset_x_px" to (x - (keyX + keyWidth / 2.0)),
            "active_key_center_offset_y_px" to (y - (keyY + keyHeight / 2.0)),
            "active_key_touch_x_ratio" to ratio(x - keyX, keyWidth),
            "active_key_touch_y_ratio" to ratio(y - keyY, keyHeight),
            "active_key_distance_to_bounds_px" to distanceToBoundsPx,
            "active_key_near_threshold_px" to nearThresholdPx,
            "active_key_inside_hitbox" to pointInRect(
                x.toDouble(),
                y.toDouble(),
                hitBox.left,
                hitBox.top,
                hitBox.right,
                hitBox.bottom
            ),
            "active_key_inside_bounds" to pointInRect(
                x.toDouble(),
                y.toDouble(),
                keyX,
                keyY,
                keyX + keyWidth,
                keyY + keyHeight
            )
        )
    }

    private fun detectHitKey(keyboard: Keyboard, x: Int, y: Int): Key? {
        var minDistance = Int.MAX_VALUE
        var primaryKey: Key? = null
        for (key in keyboard.getNearestKeys(x, y)) {
            if (key.isSpacer || !key.isOnKey(x, y)) {
                continue
            }
            val distance = key.squaredDistanceToEdge(x, y)
            if (distance > minDistance) {
                continue
            }
            if (primaryKey == null || distance < minDistance || key.code > primaryKey.code) {
                minDistance = distance
                primaryKey = key
            }
        }
        return primaryKey
    }

    private fun nearestKey(keyboard: Keyboard, x: Int, y: Int): Key? {
        var minDistance = Int.MAX_VALUE
        var nearestKey: Key? = null
        for (key in keyboard.getNearestKeys(x, y)) {
            if (key.isSpacer) {
                continue
            }
            val distance = key.squaredDistanceToEdge(x, y)
            if (distance < minDistance) {
                minDistance = distance
                nearestKey = key
            }
        }
        return nearestKey
    }

    private fun activeKeyNearThresholdPx(key: Key): Double =
        max(MIN_ACTIVE_KEY_NEAR_THRESHOLD_PX, min(key.width, key.height).toDouble() * 0.25)

    internal fun activeKeyRelationForSample(
        x: Double,
        y: Double,
        keyX: Int,
        keyY: Int,
        keyWidth: Int,
        keyHeight: Int,
        hitBoxLeft: Int,
        hitBoxTop: Int,
        hitBoxRight: Int,
        hitBoxBottom: Int,
        nearThresholdPx: Double
    ): String {
        if (pointInRect(x, y, hitBoxLeft, hitBoxTop, hitBoxRight, hitBoxBottom)) {
            return "inside_hitbox"
        }
        if (pointInRect(x, y, keyX, keyY, keyX + keyWidth, keyY + keyHeight)) {
            return "inside_key_bounds"
        }
        return if (distanceToBoundsPx(x, y, keyX, keyY, keyWidth, keyHeight) <= nearThresholdPx) {
            "near_key_bounds"
        } else {
            "outside_key_bounds"
        }
    }

    private fun pointInRect(x: Double, y: Double, left: Int, top: Int, right: Int, bottom: Int): Boolean =
        x >= left && x < right && y >= top && y < bottom

    private fun distanceToBoundsPx(
        x: Double,
        y: Double,
        keyX: Int,
        keyY: Int,
        keyWidth: Int,
        keyHeight: Int
    ): Double {
        val right = keyX + keyWidth
        val bottom = keyY + keyHeight
        val edgeX = when {
            x < keyX -> keyX.toDouble()
            x >= right -> right.toDouble()
            else -> x
        }
        val edgeY = when {
            y < keyY -> keyY.toDouble()
            y >= bottom -> bottom.toDouble()
            else -> y
        }
        val dx = x - edgeX
        val dy = y - edgeY
        return sqrt(dx * dx + dy * dy)
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

    private fun inputScopeStatus(context: Context, active: Boolean): InputScopeStatus {
        val fieldSnapshot = fieldSnapshot()
        val ready = isEnabled(context) && active && fieldSnapshot != null
        val state = when {
            !isEnabled(context) -> "logging_disabled"
            !active -> "session_inactive"
            currentInputAttributes == null -> "no_current_field"
            !knownNonPasswordField -> "no_non_password_field"
            currentFieldEpisodeId == null -> "no_field_episode"
            else -> "ready"
        }
        return InputScopeStatus(ready, state, if (ready) fieldSnapshot else null)
    }

    private fun currentSessionSnapshot(context: Context): SessionSnapshot? {
        val sessionId = currentSessionId(context) ?: return null
        return SessionSnapshot(
            sessionId,
            currentExternalRunId(context),
            currentInputActor(context),
            currentInputController(context),
            currentInputCadencePolicy(context),
            currentInputProfileSource(context),
            currentInputProfileId(context),
            currentInputProfileSchema(context),
            currentInputProfileHash(context),
            currentInputProfileSeed(context)
        )
    }

    private fun fieldSnapshot(): FieldSnapshot? {
        val inputAttributes = currentInputAttributes ?: return null
        if (!knownNonPasswordField) return null
        val episodeId = currentFieldEpisodeId ?: return null
        return FieldSnapshot(inputAttributes.mTargetApplicationPackageName, episodeId)
    }

    private fun beginFieldEpisode(inputAttributes: InputAttributes): FieldSnapshot {
        val signature = fieldSignature(inputAttributes)
        val now = SystemClock.uptimeMillis()
        val existingEpisodeId = currentFieldEpisodeId
        val episodeId = when {
            existingEpisodeId != null && currentFieldSignature == signature -> existingEpisodeId
            lastFinishedFieldEpisodeId != null &&
                    lastFinishedFieldSignature == signature &&
                    now - lastFinishedFieldUptimeMs <= FIELD_EPISODE_REUSE_WINDOW_MS ->
                lastFinishedFieldEpisodeId ?: nextFieldEpisodeId()
            else -> nextFieldEpisodeId()
        }
        currentFieldEpisodeId = episodeId
        currentFieldSignature = signature
        return FieldSnapshot(inputAttributes.mTargetApplicationPackageName, episodeId)
    }

    private fun rememberFinishedFieldEpisode() {
        val episodeId = currentFieldEpisodeId ?: return
        val signature = currentFieldSignature ?: return
        lifecycleFieldSnapshot = fieldSnapshot()
        lastFinishedFieldEpisodeId = episodeId
        lastFinishedFieldSignature = signature
        lastFinishedFieldUptimeMs = SystemClock.uptimeMillis()
    }

    private fun clearFieldEpisodeState(clearLastFinished: Boolean) {
        currentFieldEpisodeId = null
        currentFieldSignature = null
        lifecycleFieldSnapshot = null
        currentInputAttributes = null
        knownNonPasswordField = false
        if (clearLastFinished) {
            lastFinishedFieldEpisodeId = null
            lastFinishedFieldSignature = null
            lastFinishedFieldUptimeMs = 0L
        }
    }

    private fun resetFieldEpisodeIds() {
        currentFieldEpisodeId = null
        currentFieldSignature = null
        lastFinishedFieldEpisodeId = null
        lastFinishedFieldSignature = null
        lastFinishedFieldUptimeMs = 0L
        lifecycleFieldSnapshot = null
    }

    private fun nextFieldEpisodeId(): Long =
        fieldEpisodeCounter.incrementAndGet()

    private fun fieldSignature(inputAttributes: InputAttributes): String =
        listOf(
            inputAttributes.mTargetApplicationPackageName.orEmpty(),
            inputAttributes.mInputType.toString(),
            inputAttributes.mImeOptions.toString(),
            inputAttributes.mEffectiveImeOptions.toString(),
            inputAttributes.mImeAction.toString(),
            inputAttributes.mEditorActionId.toString(),
            inputAttributes.mEditorActionLabel.orEmpty(),
            inputAttributes.mEditorFieldId.toString()
        ).joinToString("|")

    private fun editorInfoFields(
        inputAttributes: InputAttributes?,
        includeFieldActionId: Boolean
    ): Map<String, Any?> {
        if (inputAttributes == null) return emptyMap()
        val fields = mutableMapOf<String, Any?>(
            "ime_options" to inputAttributes.mImeOptions,
            "effective_ime_options" to inputAttributes.mEffectiveImeOptions,
            "ime_action" to inputAttributes.mImeAction,
            "ime_action_name" to editorActionName(inputAttributes.mImeAction),
            "action_label" to inputAttributes.mEditorActionLabel,
            "editor_field_id" to inputAttributes.mEditorFieldId
        )
        if (includeFieldActionId) {
            fields["action_id"] = inputAttributes.mEditorActionId
        } else {
            fields["field_action_id"] = inputAttributes.mEditorActionId
        }
        return fields
    }

    private fun inputFieldFields(inputAttributes: InputAttributes): Map<String, Any?> {
        val inputType = inputAttributes.mInputType
        return mapOf(
            "input_type" to inputType,
            "input_type_class" to (inputType and InputType.TYPE_MASK_CLASS),
            "input_type_variation" to (inputType and InputType.TYPE_MASK_VARIATION),
            "input_type_flags" to (inputType and InputType.TYPE_MASK_FLAGS)
        ) + editorInfoFields(inputAttributes, includeFieldActionId = true)
    }

    private fun editorActionName(actionId: Int): String =
        when (actionId) {
            InputTypeUtils.IME_ACTION_CUSTOM_LABEL -> "actionCustomLabel"
            EditorInfo.IME_ACTION_UNSPECIFIED,
            EditorInfo.IME_ACTION_NONE,
            EditorInfo.IME_ACTION_GO,
            EditorInfo.IME_ACTION_SEARCH,
            EditorInfo.IME_ACTION_SEND,
            EditorInfo.IME_ACTION_NEXT,
            EditorInfo.IME_ACTION_DONE,
            EditorInfo.IME_ACTION_PREVIOUS -> EditorInfoCompatUtils.imeActionName(actionId)
            else -> "actionUnknown($actionId)"
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

    private fun addNanosecondCompanion(record: JSONObject, millisField: String, nanosField: String) {
        if (!record.has(millisField) || record.has(nanosField)) return
        val millis = when (val value = record.opt(millisField)) {
            is Number -> value.toLong()
            is String -> value.toLongOrNull()
            else -> null
        } ?: return
        record.put(nanosField, TimeUnit.MILLISECONDS.toNanos(millis))
    }

    private fun eventTimeMetadata(timestampSource: String): TimestampMetadata =
        uptimeMillisecondsMetadata(
            timestampSource = timestampSource,
            field = "t_event_uptime_ms",
            fieldNs = "t_event_uptime_ns"
        )

    private fun keyEventTimeMetadata(event: String): TimestampMetadata =
        eventTimeMetadata(
            when (event) {
                "key_down", "key_up", "key_commit" -> TIMESTAMP_SOURCE_MOTION_EVENT
                else -> TIMESTAMP_SOURCE_SYNTHETIC_HANDLER
            }
        )

    private fun downTimeMetadata(timestampSource: String): TimestampMetadata =
        uptimeMillisecondsMetadata(
            timestampSource = timestampSource,
            field = "t_down_uptime_ms",
            fieldNs = "t_down_uptime_ns"
        )

    private fun uptimeMillisecondsMetadata(
        timestampSource: String,
        field: String,
        fieldNs: String
    ): TimestampMetadata =
        TimestampMetadata(
            clockDomain = CLOCK_DOMAIN_ANDROID_UPTIME_MS,
            timestampSource = timestampSource,
            timestampPrecision = TIMESTAMP_PRECISION_MILLISECONDS,
            field = field,
            fieldNs = fieldNs,
            fieldNsPrecision = TIMESTAMP_PRECISION_MILLISECONDS_CONVERTED_TO_NANOSECONDS
        )

    private fun captureTimeMetadata(): TimestampMetadata =
        TimestampMetadata(
            clockDomain = CLOCK_DOMAIN_DEVICE_ELAPSED_REALTIME_NS,
            timestampSource = TIMESTAMP_SOURCE_CALLBACK_CAPTURE,
            timestampPrecision = TIMESTAMP_PRECISION_NANOSECONDS,
            field = "t_capture_elapsed_realtime_ns"
        )

    private fun writeTimeMetadata(): TimestampMetadata =
        TimestampMetadata(
            clockDomain = CLOCK_DOMAIN_DEVICE_ELAPSED_REALTIME_NS,
            timestampSource = TIMESTAMP_SOURCE_WRITER,
            timestampPrecision = TIMESTAMP_PRECISION_NANOSECONDS,
            field = "t_elapsed_realtime_ns"
        )

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

    private fun toolTypeName(toolType: Int): String =
        when (toolType) {
            MotionEvent.TOOL_TYPE_UNKNOWN -> "unknown"
            MotionEvent.TOOL_TYPE_FINGER -> "finger"
            MotionEvent.TOOL_TYPE_STYLUS -> "stylus"
            MotionEvent.TOOL_TYPE_MOUSE -> "mouse"
            MotionEvent.TOOL_TYPE_ERASER -> "eraser"
            else -> "other"
        }

    private fun motionClassificationName(classification: Int): String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            when (classification) {
                MotionEvent.CLASSIFICATION_NONE -> "none"
                MotionEvent.CLASSIFICATION_AMBIGUOUS_GESTURE -> "ambiguous_gesture"
                MotionEvent.CLASSIFICATION_DEEP_PRESS -> "deep_press"
                else -> "other"
            }
        } else {
            "unavailable"
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

    private fun currentOptionalString(context: Context, key: String): String? =
        context.prefs().getString(key, null)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }

    private fun SharedPreferences.Editor.putOptionalString(key: String, value: String?) {
        if (value == null) {
            remove(key)
        } else {
            putString(key, value)
        }
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
        val inputCadencePolicy: String,
        val inputProfileSource: String?,
        val inputProfileId: String?,
        val inputProfileSchema: String?,
        val inputProfileHash: String?,
        val inputProfileSeed: String?
    )

    private data class PendingEvent(
        val name: String,
        val fields: Map<String, Any?>,
        val eventTime: TimestampMetadata? = null,
        val downTime: TimestampMetadata? = null
    )

    private data class CapturedPendingEvent(
        val event: PendingEvent,
        val captureTimestamp: CaptureTimestamp
    )

    private data class CaptureTimestamp(
        val elapsedRealtimeNs: Long
    ) {
        companion object {
            fun now(): CaptureTimestamp =
                CaptureTimestamp(SystemClock.elapsedRealtimeNanos())
        }
    }

    private data class TimestampMetadata(
        val clockDomain: String,
        val timestampSource: String,
        val timestampPrecision: String,
        val field: String,
        val fieldNs: String? = null,
        val fieldNsPrecision: String? = null
    ) {
        fun toJson(): JSONObject {
            val json = JSONObject()
                .put("clock_domain", clockDomain)
                .put("timestamp_source", timestampSource)
                .put("timestamp_precision", timestampPrecision)
                .put("field", field)
            if (fieldNs != null) {
                json.put("field_ns", fieldNs)
            }
            if (fieldNsPrecision != null) {
                json.put("field_ns_precision", fieldNsPrecision)
            }
            return json
        }
    }

    private data class FieldSnapshot(
        val targetPackage: String?,
        val fieldEpisodeId: Long
    )

    private data class InputScopeStatus(
        val ready: Boolean,
        val state: String,
        val fieldSnapshot: FieldSnapshot?
    )
}
