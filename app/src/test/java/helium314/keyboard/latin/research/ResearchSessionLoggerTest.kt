// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Intent
import android.text.InputType
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.inputmethod.EditorInfo
import androidx.test.core.app.ApplicationProvider
import helium314.keyboard.ShadowInputMethodManager2
import helium314.keyboard.latin.App
import helium314.keyboard.latin.InputAttributes
import helium314.keyboard.latin.utils.InputTypeUtils
import org.json.JSONObject
import org.junit.Before
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

@RunWith(RobolectricTestRunner::class)
@Config(shadows = [
    ShadowInputMethodManager2::class,
])
class ResearchSessionLoggerTest {
    private val context: App
        get() = ApplicationProvider.getApplicationContext()

    @Before
    fun setUp() {
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()
        ResearchSessionLogger.setEnabled(context, false)
        ResearchSessionLogger.deleteAllLogs(context)
        ResearchSessionLogger.onInputFieldFinished(context)
        ResearchSessionLogger.waitForPendingWrites()
    }

    @Test
    fun `external run id is written to every record in a session`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-test-001")
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        ResearchSessionLogger.onInputFieldFinished(context)
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val records = readRecords(sessionId)
        assertTrue(records.any { it.getString("event") == "session_start" })
        assertTrue(records.any { it.getString("event") == "session_stop" })
        assertTrue(records.any { it.optString("target_package") == "org.example.input" })
        assertTrue(records.any { it.has("field_episode_id") })
        records.forEach { record ->
            assertEquals("input_dynamics_event.v1", record.getString("schema"))
            assertEquals("run-test-001", record.getString("external_run_id"))
            assertTimestampFields(record)
            assertFalse(record.optBoolean("password_field", false))
        }
    }

    @Test
    fun `password fields suppress input scoped records`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-password-test")
        ResearchSessionLogger.onInputFieldStarted(context, passwordAttributes())
        ResearchSessionLogger.logEvent(context, "manual_probe")
        ResearchSessionLogger.onInputFieldFinished(context)
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val records = readRecords(sessionId)
        assertEquals(listOf("session_start", "session_stop"), records.map { it.getString("event") })
        records.forEach { record ->
            assertEquals("run-password-test", record.getString("external_run_id"))
            assertFalse(record.optBoolean("password_field", false))
        }
    }

    @Test
    fun `pointer samples include motion and coordinate frame metadata`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-pointer-frame-test")
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        val keyboardView = View(context).apply {
            layout(0, 0, 100, 200)
        }
        val event = motionEvent()
        try {
            ResearchSessionLogger.logMotionEvent(context, event, keyboardView)
        } finally {
            event.recycle()
        }
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val pointerSample = readRecords(sessionId)
            .first { it.getString("event") == "pointer_sample" }
        assertEquals("down", pointerSample.getString("motion_action_name"))
        assertEquals(MotionEvent.ACTION_DOWN, pointerSample.getInt("motion_action"))
        assertEquals(0, pointerSample.getInt("motion_action_index"))
        assertEquals(1, pointerSample.getInt("pointer_count"))
        assertEquals(MotionEvent.TOOL_TYPE_FINGER, pointerSample.getInt("tool_type"))
        assertEquals("finger", pointerSample.getString("tool_type_name"))
        assertEquals("keyboard_view_local_px", pointerSample.getString("coordinate_space"))
        assertTrue(pointerSample.getBoolean("coordinate_frame_available"))
        assertEquals(100, pointerSample.getInt("keyboard_view_width_px"))
        assertEquals(200, pointerSample.getInt("keyboard_view_height_px"))
        assertEquals(12.0, pointerSample.getDouble("x_screen_px"))
        assertEquals(34.0, pointerSample.getDouble("y_screen_px"))
        assertTrue(pointerSample.has("display_width_px"))
        assertTrue(pointerSample.has("display_height_px"))
        assertFalse(pointerSample.getBoolean("active_key_present"))
        assertEquals("keyboard_unavailable", pointerSample.getString("active_key_lookup"))
        assertEquals(1_010_000_000L, pointerSample.getLong("t_event_uptime_ns"))
        assertEquals(1_000_000_000L, pointerSample.getLong("t_down_uptime_ns"))
        assertTimestampClaim(
            pointerSample.getJSONObject("event_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "motion_event",
            timestampPrecision = "milliseconds",
            field = "t_event_uptime_ms",
            fieldNs = "t_event_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
        assertTimestampClaim(
            pointerSample.getJSONObject("down_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "motion_event",
            timestampPrecision = "milliseconds",
            field = "t_down_uptime_ms",
            fieldNs = "t_down_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
        assertFalse(pointerSample.getBoolean("keyboard_state_available"))
        assertEquals("keyboard_unavailable", pointerSample.getString("keyboard_state_unavailable_reason"))
        assertTrue(pointerSample.has("keyboard_mode"))
        assertTrue(pointerSample.has("keyboard_element_id"))
        assertTrue(pointerSample.has("keyboard_shift_mode"))
        assertTrue(pointerSample.has("keyboard_subtype_main_layout_name"))
    }

    @Test
    fun `key records include event timestamp nanosecond companion`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-key-timestamp-test")
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        ResearchSessionLogger.logKeyEvent(
            "key_down",
            pointerId = 0,
            x = 10,
            y = 20,
            eventTime = 1_234L,
            key = null
        )
        ResearchSessionLogger.logKeyEvent(
            "key_repeat",
            pointerId = 0,
            x = 10,
            y = 20,
            eventTime = 1_240L,
            key = null
        )
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val keyDown = readRecords(sessionId).first { it.getString("event") == "key_down" }
        assertTimestampFields(keyDown)
        assertEquals(1_234L, keyDown.getLong("t_event_uptime_ms"))
        assertEquals(1_234_000_000L, keyDown.getLong("t_event_uptime_ns"))
        assertTimestampClaim(
            keyDown.getJSONObject("event_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "motion_event",
            timestampPrecision = "milliseconds",
            field = "t_event_uptime_ms",
            fieldNs = "t_event_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
        val keyRepeat = readRecords(sessionId).first { it.getString("event") == "key_repeat" }
        assertTimestampFields(keyRepeat)
        assertEquals(1_240L, keyRepeat.getLong("t_event_uptime_ms"))
        assertEquals(1_240_000_000L, keyRepeat.getLong("t_event_uptime_ns"))
        assertTimestampClaim(
            keyRepeat.getJSONObject("event_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "synthetic_handler",
            timestampPrecision = "milliseconds",
            field = "t_event_uptime_ms",
            fieldNs = "t_event_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
        assertFalse(keyDown.getBoolean("keyboard_state_available"))
        assertTrue(keyDown.has("keyboard_state_unavailable_reason"))
        assertTrue(keyDown.has("keyboard_mode"))
        assertTrue(keyDown.has("keyboard_element_id"))
        assertTrue(keyDown.has("keyboard_shift_mode"))
        assertTrue(keyDown.has("keyboard_subtype_main_layout_name"))
    }

    @Test
    fun `system back records include key event timestamp metadata`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-system-back-test")
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        ResearchSessionLogger.onSystemBackKeyEvent(
            context,
            keyAction = "down",
            keyCode = KeyEvent.KEYCODE_BACK,
            eventTime = 2_345L,
            repeatCount = 0,
            canceled = false
        )
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val systemBack = readRecords(sessionId).first { it.getString("event") == "system_back_event" }
        assertTimestampFields(systemBack)
        assertEquals(2_345L, systemBack.getLong("t_event_uptime_ms"))
        assertEquals(2_345_000_000L, systemBack.getLong("t_event_uptime_ns"))
        assertTimestampClaim(
            systemBack.getJSONObject("event_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "key_event",
            timestampPrecision = "milliseconds",
            field = "t_event_uptime_ms",
            fieldNs = "t_event_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
    }

    @Test
    fun `active key relation classifies sample position against hitbox and bounds`() {
        assertEquals(
            "inside_hitbox",
            ResearchSessionLogger.activeKeyRelationForSample(
                x = 12.0,
                y = 12.0,
                keyX = 10,
                keyY = 10,
                keyWidth = 20,
                keyHeight = 20,
                hitBoxLeft = 8,
                hitBoxTop = 8,
                hitBoxRight = 32,
                hitBoxBottom = 32,
                nearThresholdPx = 5.0
            )
        )
        assertEquals(
            "near_key_bounds",
            ResearchSessionLogger.activeKeyRelationForSample(
                x = 34.0,
                y = 20.0,
                keyX = 10,
                keyY = 10,
                keyWidth = 20,
                keyHeight = 20,
                hitBoxLeft = 10,
                hitBoxTop = 10,
                hitBoxRight = 30,
                hitBoxBottom = 30,
                nearThresholdPx = 5.0
            )
        )
        assertEquals(
            "outside_key_bounds",
            ResearchSessionLogger.activeKeyRelationForSample(
                x = 50.0,
                y = 20.0,
                keyX = 10,
                keyY = 10,
                keyWidth = 20,
                keyHeight = 20,
                hitBoxLeft = 10,
                hitBoxTop = 10,
                hitBoxRight = 30,
                hitBoxBottom = 30,
                nearThresholdPx = 5.0
            )
        )
    }

    @Test
    fun `field episode id and editor metadata are written for non-password fields`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-field-episode-test")
        val attributes = textAttributes(
            imeOptions = EditorInfo.IME_ACTION_SEARCH,
            actionId = 77,
            actionLabel = "Search",
            fieldId = 42
        )
        ResearchSessionLogger.onInputFieldStarted(context, attributes)
        ResearchSessionLogger.onEditorAction(context, EditorInfo.IME_ACTION_SEARCH)
        ResearchSessionLogger.onInputFieldFinished(context)
        ResearchSessionLogger.onInputFieldStarted(context, attributes)
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val records = readRecords(sessionId)
        val fieldEnterRecords = records.filter { it.getString("event") == "field_enter" }
        assertEquals(2, fieldEnterRecords.size)
        val episodeId = fieldEnterRecords.first().getLong("field_episode_id")
        assertTrue(episodeId > 0)
        assertEquals(episodeId, fieldEnterRecords.last().getLong("field_episode_id"))
        assertEquals(EditorInfo.IME_ACTION_SEARCH, fieldEnterRecords.first().getInt("ime_options"))
        assertEquals(InputTypeUtils.IME_ACTION_CUSTOM_LABEL, fieldEnterRecords.first().getInt("ime_action"))
        assertEquals("actionCustomLabel", fieldEnterRecords.first().getString("ime_action_name"))
        assertEquals("Search", fieldEnterRecords.first().getString("action_label"))
        assertEquals(77, fieldEnterRecords.first().getInt("action_id"))
        assertEquals(42, fieldEnterRecords.first().getInt("editor_field_id"))
        assertFalse(fieldEnterRecords.first().has("event_time"))
        assertFalse(fieldEnterRecords.first().has("down_time"))
        assertFalse(fieldEnterRecords.first().getBoolean("keyboard_state_available"))
        assertTrue(fieldEnterRecords.first().has("keyboard_shift_mode"))

        val editorAction = records.first { it.getString("event") == "editor_action" }
        assertEquals(episodeId, editorAction.getLong("field_episode_id"))
        assertEquals(EditorInfo.IME_ACTION_SEARCH, editorAction.getInt("action_id"))
        assertFalse(editorAction.has("event_time"))
        assertFalse(editorAction.has("down_time"))
        assertEquals("actionSearch", editorAction.getString("action_name"))
        assertEquals(EditorInfo.IME_ACTION_SEARCH, editorAction.getInt("ime_options"))
        assertEquals(InputTypeUtils.IME_ACTION_CUSTOM_LABEL, editorAction.getInt("ime_action"))
        assertEquals("Search", editorAction.getString("action_label"))
        assertEquals(77, editorAction.getInt("field_action_id"))
    }

    @Test
    fun `starting session preserves already active non-password field snapshot`() {
        ResearchSessionLogger.setEnabled(context, true)
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        val sessionId = ResearchSessionLogger.startSession(context, "run-active-field-test")
        ResearchSessionLogger.logEvent(context, "manual_probe")
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val manualProbe = readRecords(sessionId).first { it.getString("event") == "manual_probe" }
        assertEquals("org.example.input", manualProbe.getString("target_package"))
        assertEquals(1, manualProbe.getLong("field_episode_id"))
        assertFalse(manualProbe.getBoolean("password_field"))
    }

    @Test
    fun `stopping a session does not discard active non-password field scope`() {
        ResearchSessionLogger.setEnabled(context, true)
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        val firstSessionId = ResearchSessionLogger.startSession(context, "run-restart-field-first")
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val secondSessionId = ResearchSessionLogger.startSession(context, "run-restart-field-second")
        val status = ResearchSessionLogger.controlStatusJson(context)
        ResearchSessionLogger.logEvent(context, "manual_probe")
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val firstRecords = readRecords(firstSessionId)
        assertEquals(listOf("session_start", "field_enter", "session_stop"), firstRecords.map { it.getString("event") })

        assertTrue(status.getBoolean("input_scope_ready"))
        assertEquals("ready", status.getString("input_scope_state"))
        assertEquals("org.example.input", status.getString("current_target_package"))
        assertEquals(1L, status.getLong("current_field_episode_id"))

        val secondRecords = readRecords(secondSessionId)
        assertEquals(
            listOf("session_start", "field_enter", "manual_probe", "session_stop"),
            secondRecords.map { it.getString("event") }
        )
        val fieldEnter = secondRecords.first { it.getString("event") == "field_enter" }
        assertEquals("session_start_current_field", fieldEnter.getString("field_enter_source"))
        val manualProbe = secondRecords.first { it.getString("event") == "manual_probe" }
        assertEquals("org.example.input", manualProbe.getString("target_package"))
        assertEquals(1L, manualProbe.getLong("field_episode_id"))
        secondRecords.forEach { record ->
            assertEquals("run-restart-field-second", record.getString("external_run_id"))
            assertFalse(record.optBoolean("password_field", false))
        }
    }

    @Test
    fun `control status includes canonical device clock probe`() {
        val status = ResearchSessionLogger.controlStatusJson(
            context,
            requestId = "request-clock-probe",
            command = "status",
            pendingWritesDrained = true
        )

        val probe = status.getJSONObject("device_clock_probe")
        assertEquals("input_dynamics_device_clock_probe.v1", probe.getString("schema"))
        assertEquals("request-clock-probe", probe.getString("request_id"))
        assertEquals("status_broadcast", probe.getString("probe_source"))
        assertEquals("android_control_status", probe.getString("captured_by"))
        assertEquals("device_elapsed_realtime_ns", probe.getString("canonical_clock_domain"))
        assertTrue(probe.getBoolean("pending_writes_drained"))
        assertEquals(probe.getLong("t_wall_ms"), status.getLong("t_wall_ms"))
        assertEquals(probe.getLong("t_uptime_ms"), status.getLong("t_uptime_ms"))
        assertEquals(probe.getLong("t_uptime_ns"), status.getLong("t_uptime_ns"))
        assertEquals(probe.getLong("t_elapsed_realtime_ns"), status.getLong("t_elapsed_realtime_ns"))
        assertEquals(probe.getLong("t_uptime_ms") * 1_000_000L, probe.getLong("t_uptime_ns"))
        assertTrue(probe.getLong("t_elapsed_realtime_ns") > 0L)
        assertTrue(probe.getLong("t_wall_ms") > 0L)
        assertEquals("diagnostic", probe.getString("wall_time_role"))
        assertTrue(status.getBoolean("pending_writes_drained"))
        assertTrue(status.has("android_sdk_int"))

        assertTimestampClaim(
            probe.getJSONObject("uptime_time"),
            clockDomain = "android_uptime_ms",
            timestampSource = "callback_capture",
            timestampPrecision = "milliseconds",
            field = "t_uptime_ms",
            fieldNs = "t_uptime_ns",
            fieldNsPrecision = "milliseconds_converted_to_nanoseconds"
        )
        assertTimestampClaim(
            probe.getJSONObject("elapsed_realtime_time"),
            clockDomain = "device_elapsed_realtime_ns",
            timestampSource = "callback_capture",
            timestampPrecision = "nanoseconds",
            field = "t_elapsed_realtime_ns"
        )
        assertTimestampClaim(
            probe.getJSONObject("wall_time"),
            clockDomain = "device_wall_ms",
            timestampSource = "callback_capture",
            timestampPrecision = "milliseconds",
            field = "t_wall_ms"
        )
    }

    @Test
    fun `control status clock probes are nondecreasing`() {
        val first = ResearchSessionLogger.controlStatusJson(context)
            .getJSONObject("device_clock_probe")
        val second = ResearchSessionLogger.controlStatusJson(context)
            .getJSONObject("device_clock_probe")

        assertTrue(second.getLong("t_uptime_ms") >= first.getLong("t_uptime_ms"))
        assertTrue(second.getLong("t_uptime_ns") >= first.getLong("t_uptime_ns"))
        assertTrue(second.getLong("t_elapsed_realtime_ns") >= first.getLong("t_elapsed_realtime_ns"))
    }

    @Test
    fun `control receiver writes request correlated status result`() {
        val requestId = "request-receiver-test"
        val receiver = ResearchControlReceiver()

        receiver.onReceive(
            context,
            Intent(ResearchControlReceiver.ACTION_STATUS)
                .putExtra(ResearchControlReceiver.EXTRA_REQUEST_ID, requestId)
        )

        val resultFile = ResearchSessionLogger.logDirectory(context)
            .resolve("input_dynamics_control_result_$requestId.json")
        assertTrue(
            resultFile.exists(),
            ResearchSessionLogger.logDirectory(context).listFiles()
                ?.map { it.name }
                ?.sorted()
                .orEmpty()
                .joinToString(", ")
        )
        val result = JSONObject(resultFile.readText())
        val probe = result.getJSONObject("device_clock_probe")
        assertEquals(requestId, result.getString("request_id"))
        assertEquals(requestId, probe.getString("request_id"))
        assertEquals("input_dynamics_device_clock_probe.v1", probe.getString("schema"))
        assertTrue(result.getBoolean("pending_writes_drained"))
        assertTrue(probe.getBoolean("pending_writes_drained"))
    }

    @Test
    fun `text edit operation records are field scoped and password suppressed`() {
        ResearchSessionLogger.setEnabled(context, true)
        val sessionId = ResearchSessionLogger.startSession(context, "run-text-edit-test")
        ResearchSessionLogger.onInputFieldStarted(context, textAttributes())
        ResearchSessionLogger.logEvent(context, "commit_text", mapOf(
            "commit_text" to "ab",
            "commit_text_length" to 2,
            "new_cursor_position" to 1,
            "selection_start_before" to 0,
            "selection_end_before" to 0,
            "selection_start_after" to 2,
            "selection_end_after" to 2
        ))
        ResearchSessionLogger.onInputFieldFinished(context)
        ResearchSessionLogger.onInputFieldStarted(context, passwordAttributes())
        ResearchSessionLogger.logEvent(context, "delete_surrounding_text", mapOf(
            "delete_before_count" to 1,
            "delete_after_count" to 0
        ))
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val records = readRecords(sessionId)
        val commitText = records.first { it.getString("event") == "commit_text" }
        assertEquals("ab", commitText.getString("commit_text"))
        assertEquals(2, commitText.getInt("commit_text_length"))
        assertEquals(1, commitText.getLong("field_episode_id"))
        assertEquals("org.example.input", commitText.getString("target_package"))
        assertFalse(commitText.getBoolean("password_field"))
        assertFalse(records.any { it.getString("event") == "delete_surrounding_text" })
    }

    private fun textAttributes(
        imeOptions: Int = 0,
        actionId: Int = 0,
        actionLabel: CharSequence? = null,
        fieldId: Int = 0
    ): InputAttributes =
        InputAttributes(
            EditorInfo().apply {
                packageName = "org.example.input"
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_NORMAL
                this.imeOptions = imeOptions
                this.actionId = actionId
                this.actionLabel = actionLabel
                this.fieldId = fieldId
            },
            false,
            context.packageName
        )

    private fun passwordAttributes(): InputAttributes =
        InputAttributes(
            EditorInfo().apply {
                packageName = "org.example.input"
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            },
            false,
            context.packageName
        )

    private fun motionEvent(): MotionEvent {
        val pointerProperties = arrayOf(
            MotionEvent.PointerProperties().apply {
                id = 0
                toolType = MotionEvent.TOOL_TYPE_FINGER
            }
        )
        val pointerCoords = arrayOf(
            MotionEvent.PointerCoords().apply {
                x = 12f
                y = 34f
                pressure = 0.5f
                size = 0.25f
            }
        )
        return MotionEvent.obtain(
            1_000L,
            1_010L,
            MotionEvent.ACTION_DOWN,
            1,
            pointerProperties,
            pointerCoords,
            0,
            0,
            1f,
            1f,
            7,
            0,
            0,
            0
        )
    }

    private fun readRecords(sessionId: String): List<JSONObject> {
        val file = ResearchSessionLogger.logDirectory(context).resolve("session-$sessionId.jsonl")
        return file.readLines().filter { it.isNotBlank() }.map { JSONObject(it) }
    }

    private fun assertTimestampFields(record: JSONObject) {
        assertTrue(record.has("t_wall_ms"))
        assertTrue(record.has("t_uptime_ms"))
        assertTrue(record.has("t_uptime_ns"))
        assertTrue(record.has("t_elapsed_realtime_ns"))
        assertTrue(record.has("t_capture_elapsed_realtime_ns"))
        assertTrue(record.has("capture_time"))
        assertTrue(record.has("write_time"))
        assertEquals(record.getLong("t_uptime_ms") * 1_000_000L, record.getLong("t_uptime_ns"))
        assertTrue(record.getLong("t_elapsed_realtime_ns") > 0L)
        assertTrue(record.getLong("t_capture_elapsed_realtime_ns") > 0L)
        assertTrue(record.getLong("t_capture_elapsed_realtime_ns") <= record.getLong("t_elapsed_realtime_ns"))
        assertTimestampClaim(
            record.getJSONObject("capture_time"),
            clockDomain = "device_elapsed_realtime_ns",
            timestampSource = "callback_capture",
            timestampPrecision = "nanoseconds",
            field = "t_capture_elapsed_realtime_ns"
        )
        assertTimestampClaim(
            record.getJSONObject("write_time"),
            clockDomain = "device_elapsed_realtime_ns",
            timestampSource = "writer",
            timestampPrecision = "nanoseconds",
            field = "t_elapsed_realtime_ns"
        )
    }

    private fun assertTimestampClaim(
        claim: JSONObject,
        clockDomain: String,
        timestampSource: String,
        timestampPrecision: String,
        field: String,
        fieldNs: String? = null,
        fieldNsPrecision: String? = null
    ) {
        assertEquals(clockDomain, claim.getString("clock_domain"))
        assertEquals(timestampSource, claim.getString("timestamp_source"))
        assertEquals(timestampPrecision, claim.getString("timestamp_precision"))
        assertEquals(field, claim.getString("field"))
        if (fieldNs == null) {
            assertFalse(claim.has("field_ns"))
        } else {
            assertEquals(fieldNs, claim.getString("field_ns"))
        }
        if (fieldNsPrecision == null) {
            assertFalse(claim.has("field_ns_precision"))
        } else {
            assertEquals(fieldNsPrecision, claim.getString("field_ns_precision"))
        }
    }
}
