// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.text.InputType
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
        ResearchSessionLogger.stopSession(context)
        ResearchSessionLogger.waitForPendingWrites()

        val keyDown = readRecords(sessionId).first { it.getString("event") == "key_down" }
        assertTimestampFields(keyDown)
        assertEquals(1_234L, keyDown.getLong("t_event_uptime_ms"))
        assertEquals(1_234_000_000L, keyDown.getLong("t_event_uptime_ns"))
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

        val editorAction = records.first { it.getString("event") == "editor_action" }
        assertEquals(episodeId, editorAction.getLong("field_episode_id"))
        assertEquals(EditorInfo.IME_ACTION_SEARCH, editorAction.getInt("action_id"))
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
        assertEquals(record.getLong("t_uptime_ms") * 1_000_000L, record.getLong("t_uptime_ns"))
        assertTrue(record.getLong("t_elapsed_realtime_ns") > 0L)
    }
}
