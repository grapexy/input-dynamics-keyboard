// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.text.InputType
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

    private fun readRecords(sessionId: String): List<JSONObject> {
        val file = ResearchSessionLogger.logDirectory(context).resolve("session-$sessionId.jsonl")
        return file.readLines().filter { it.isNotBlank() }.map { JSONObject(it) }
    }
}
