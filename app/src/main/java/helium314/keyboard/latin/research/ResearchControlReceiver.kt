// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.app.Activity
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Bundle
import org.json.JSONObject

open class ResearchControlReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val appContext = context.applicationContext
        val action = intent.action.orEmpty()
        val command = action.substringAfterLast('.', action).lowercase()
        val requestId = intent.getStringExtra(EXTRA_REQUEST_ID)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
        val result = handleCommand(appContext, action, intent)
        val pendingWritesDrained = ResearchSessionLogger.waitForPendingWrites()
        val status = ResearchSessionLogger.controlStatusJson(
            appContext,
            requestId = requestId,
            command = command,
            ok = result.ok,
            message = result.message,
            includeLogs = result.includeLogs,
            pendingWritesDrained = pendingWritesDrained,
            extraFields = result.extraFields,
        )
        ResearchSessionLogger.writeControlStatusJson(appContext, status)
        ResearchSessionLogger.writeControlResultJson(appContext, requestId, status)
        publishResult(status, result.ok)
    }

    private fun handleCommand(context: Context, action: String, intent: Intent): CommandResult =
        when (action) {
            ACTION_ENABLE -> {
                ResearchSessionLogger.setEnabled(context, true)
                CommandResult(message = "input dynamics logging enabled")
            }
            ACTION_DISABLE -> {
                val stoppedSessionId = if (ResearchSessionLogger.isSessionActive(context)) {
                    ResearchSessionLogger.stopSession(context)
                } else {
                    null
                }
                ResearchSessionLogger.setEnabled(context, false)
                CommandResult(
                    message = "input dynamics logging disabled",
                    extraFields = mapOf("stopped_session_id" to stoppedSessionId)
                )
            }
            ACTION_START -> {
                if (!ResearchSessionLogger.isEnabled(context)) {
                    CommandResult(
                        ok = false,
                        message = "input dynamics logging is disabled; run ENABLE before START"
                    )
                } else {
                    val stoppedSessionId = if (ResearchSessionLogger.isSessionActive(context)) {
                        ResearchSessionLogger.stopSession(context)
                    } else {
                        null
                    }
                    ResearchSessionLogger.waitForPendingWrites()
                    val externalRunId = intent.getStringExtra(EXTRA_RUN_ID)
                        ?.trim()
                        ?.takeIf { it.isNotEmpty() }
                    val inputActor = intent.getStringExtra(EXTRA_INPUT_ACTOR)
                    val inputController = intent.getStringExtra(EXTRA_INPUT_CONTROLLER)
                    val inputCadencePolicy = intent.getStringExtra(EXTRA_INPUT_CADENCE_POLICY)
                    val inputProfileSource = intent.getStringExtra(EXTRA_INPUT_PROFILE_SOURCE)
                    val inputProfileId = intent.getStringExtra(EXTRA_INPUT_PROFILE_ID)
                    val inputProfileSchema = intent.getStringExtra(EXTRA_INPUT_PROFILE_SCHEMA)
                    val inputProfileHash = intent.getStringExtra(EXTRA_INPUT_PROFILE_HASH)
                    val inputProfileSeed = intent.getStringExtra(EXTRA_INPUT_PROFILE_SEED)
                    val sessionId = ResearchSessionLogger.startSession(
                        context,
                        externalRunId,
                        inputActor,
                        inputController,
                        inputCadencePolicy,
                        inputProfileSource,
                        inputProfileId,
                        inputProfileSchema,
                        inputProfileHash,
                        inputProfileSeed
                    )
                    CommandResult(
                        message = "input dynamics session started",
                        extraFields = mapOf(
                            "started_session_id" to sessionId,
                            "stopped_previous_session_id" to stoppedSessionId
                        )
                    )
                }
            }
            ACTION_STOP -> {
                val stoppedSessionId = if (ResearchSessionLogger.isSessionActive(context)) {
                    ResearchSessionLogger.stopSession(context)
                } else {
                    null
                }
                CommandResult(
                    message = if (stoppedSessionId == null) {
                        "no active input dynamics session"
                    } else {
                        "input dynamics session stopped"
                    },
                    extraFields = mapOf("stopped_session_id" to stoppedSessionId)
                )
            }
            ACTION_STATUS -> CommandResult(message = "input dynamics logging status")
            ACTION_KEYBOARD_LAYOUT -> CommandResult(
                message = "input dynamics keyboard layout",
                extraFields = mapOf(
                    "keyboard_layout" to ResearchKeyboardLayoutSnapshot.currentLayoutJson(context)
                )
            )
            ACTION_LIST_LOGS -> CommandResult(
                message = "input dynamics log list",
                includeLogs = true
            )
            ACTION_CLEAR_LOGS -> {
                if (ResearchSessionLogger.isSessionActive(context)) {
                    CommandResult(
                        ok = false,
                        message = "cannot clear logs while an input dynamics session is active"
                    )
                } else {
                    val deleted = ResearchSessionLogger.deleteAllLogs(context)
                    CommandResult(
                        message = "input dynamics logs cleared",
                        extraFields = mapOf("deleted_log_count" to deleted),
                        includeLogs = true
                    )
                }
            }
            else -> CommandResult(
                ok = false,
                message = "unknown input dynamics control action"
            )
        }

    private fun publishResult(status: JSONObject, ok: Boolean) {
        if (!isOrderedBroadcast) return
        val statusString = status.toString()
        setResultCode(if (ok) Activity.RESULT_OK else Activity.RESULT_CANCELED)
        setResultData(statusString)
        setResultExtras(Bundle().apply {
            putString(EXTRA_STATUS_JSON, statusString)
        })
    }

    private data class CommandResult(
        val ok: Boolean = true,
        val message: String,
        val includeLogs: Boolean = false,
        val extraFields: Map<String, Any?> = emptyMap(),
    )

    companion object {
        const val ACTION_ENABLE = "org.inputdynamics.ime.action.ENABLE"
        const val ACTION_DISABLE = "org.inputdynamics.ime.action.DISABLE"
        const val ACTION_START = "org.inputdynamics.ime.action.START"
        const val ACTION_STOP = "org.inputdynamics.ime.action.STOP"
        const val ACTION_STATUS = "org.inputdynamics.ime.action.STATUS"
        const val ACTION_KEYBOARD_LAYOUT = "org.inputdynamics.ime.action.KEYBOARD_LAYOUT"
        const val ACTION_LIST_LOGS = "org.inputdynamics.ime.action.LIST_LOGS"
        const val ACTION_CLEAR_LOGS = "org.inputdynamics.ime.action.CLEAR_LOGS"
        const val EXTRA_REQUEST_ID = "request_id"
        const val EXTRA_RUN_ID = "run_id"
        const val EXTRA_INPUT_ACTOR = "input_actor"
        const val EXTRA_INPUT_CONTROLLER = "input_controller"
        const val EXTRA_INPUT_CADENCE_POLICY = "input_cadence_policy"
        const val EXTRA_INPUT_PROFILE_SOURCE = "input_profile_source"
        const val EXTRA_INPUT_PROFILE_ID = "input_profile_id"
        const val EXTRA_INPUT_PROFILE_SCHEMA = "input_profile_schema"
        const val EXTRA_INPUT_PROFILE_HASH = "input_profile_hash"
        const val EXTRA_INPUT_PROFILE_SEED = "input_profile_seed"
        const val EXTRA_STATUS_JSON = "status_json"
    }
}
