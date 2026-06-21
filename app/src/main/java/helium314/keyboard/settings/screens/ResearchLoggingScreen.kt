// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.settings.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import helium314.keyboard.latin.R
import helium314.keyboard.latin.research.ResearchSessionLogger
import helium314.keyboard.settings.SearchSettingsScreen
import helium314.keyboard.settings.preferences.Preference
import helium314.keyboard.settings.preferences.PreferenceCategory

@Composable
fun ResearchLoggingScreen(
    onClickBack: () -> Unit,
) {
    val context = LocalContext.current
    var refreshToken by remember { mutableIntStateOf(0) }
    fun reload() {
        refreshToken++
    }

    val enabled = remember(refreshToken) { ResearchSessionLogger.isEnabled(context) }
    val active = remember(refreshToken) { ResearchSessionLogger.isSessionActive(context) }
    val sessionId = remember(refreshToken) { ResearchSessionLogger.currentSessionId(context) }
    val logFiles = remember(refreshToken) { ResearchSessionLogger.listLogFiles(context) }
    val logDirectory = remember(refreshToken) { ResearchSessionLogger.logDirectory(context).absolutePath }
    val adbPullCommand = remember(refreshToken) { ResearchSessionLogger.adbPullCommand(context) }

    SearchSettingsScreen(
        onClickBack = onClickBack,
        title = stringResource(R.string.research_logging_screen),
        settings = emptyList(),
    ) {
        Scaffold(contentWindowInsets = WindowInsets.safeDrawing.only(WindowInsetsSides.Bottom)) { innerPadding ->
            Column(
                Modifier
                    .verticalScroll(rememberScrollState())
                    .then(Modifier.padding(innerPadding))
            ) {
                PreferenceCategory(stringResource(R.string.research_logging_section_logging))
                Preference(
                    name = stringResource(R.string.research_logging_enabled),
                    description = stringResource(R.string.research_logging_enabled_summary),
                    onClick = {
                        ResearchSessionLogger.setEnabled(context, !enabled)
                        reload()
                    },
                    icon = R.drawable.ic_settings_about_log
                ) {
                    Switch(
                        checked = enabled,
                        onCheckedChange = {
                            ResearchSessionLogger.setEnabled(context, it)
                            reload()
                        },
                    )
                }

                PreferenceCategory(stringResource(R.string.research_logging_section_session))
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                    Text(
                        text = if (active)
                            stringResource(R.string.research_logging_status_active, sessionId.orEmpty())
                        else
                            stringResource(R.string.research_logging_status_inactive),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Spacer(Modifier.height(8.dp))
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        modifier = Modifier.fillMaxWidth()
                    ) {
                        Button(
                            enabled = enabled && !active,
                            onClick = {
                                ResearchSessionLogger.startSession(context)
                                reload()
                            }
                        ) {
                            Text(stringResource(R.string.research_logging_start_session))
                        }
                        OutlinedButton(
                            enabled = active,
                            onClick = {
                                ResearchSessionLogger.stopSession(context)
                                reload()
                            }
                        ) {
                            Text(stringResource(R.string.research_logging_stop_session))
                        }
                    }
                }

                PreferenceCategory(stringResource(R.string.research_logging_section_data))
                Column(Modifier.padding(horizontal = 16.dp, vertical = 12.dp)) {
                    Text(
                        text = stringResource(R.string.research_logging_log_count, logFiles.size),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    Text(
                        text = stringResource(R.string.research_logging_path, logDirectory),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                    Text(
                        text = stringResource(R.string.research_logging_adb_pull),
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.padding(top = 12.dp),
                    )
                    SelectionContainer {
                        Text(
                            text = adbPullCommand,
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.padding(top = 4.dp),
                        )
                    }
                    OutlinedButton(
                        enabled = logFiles.isNotEmpty() && !active,
                        onClick = {
                            ResearchSessionLogger.deleteAllLogs(context)
                            reload()
                        },
                        modifier = Modifier.padding(top = 12.dp),
                    ) {
                        Text(stringResource(R.string.research_logging_clear_logs))
                    }
                }
            }
        }
    }
}
