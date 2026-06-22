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
        records.forEach { record ->
            assertEquals("input_dynamics_event.v1", record.getString("schema"))
            assertEquals("run-test-001", record.getString("external_run_id"))
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
    }

    private fun textAttributes(): InputAttributes =
        InputAttributes(
            EditorInfo().apply {
                packageName = "org.example.input"
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_NORMAL
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
}
