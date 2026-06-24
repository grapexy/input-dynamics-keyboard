//! Session-owned host process descriptors, liveness probes, and stop helpers.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{CliError, CliResult};
use crate::process::{FailureMode, SpawnedProcess, run_process, spawn_detached_process_to_files};
use crate::session_state::schema::{ProcessDescriptor, ProcessKind, ProcessState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostProcessKind {
    AdbShell,
    Host,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopMethod {
    ProcessGroupInterruptThenKill,
    ProcessGroupTerminateThenKill,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessLivenessStatus {
    Running,
    Exited,
    Missing,
    InvalidDescriptor,
    IdentityMismatch,
    ProbeFailed,
    UnsupportedPlatform,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessLiveness {
    pub(crate) status: ProcessLivenessStatus,
    pub(crate) host_pid: Option<u32>,
    pub(crate) host_process_group_id: Option<u32>,
    pub(crate) observed_process_group_id: Option<u32>,
    pub(crate) message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionProcessSpec {
    pub(crate) name: String,
    pub(crate) kind: HostProcessKind,
    pub(crate) required: bool,
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
    pub(crate) remote_command: Vec<String>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stop_method: StopMethod,
    pub(crate) expected_exit: bool,
}

#[derive(Debug)]
pub(crate) struct StartedSessionProcess {
    child: SpawnedProcess,
    descriptor: ProcessDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopPolicy {
    pub(crate) grace_timeout: Duration,
    pub(crate) poll_interval: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StopOutcome {
    pub(crate) ok: bool,
    pub(crate) method: Option<StopMethod>,
    pub(crate) initial_liveness: ProcessLiveness,
    pub(crate) final_liveness: ProcessLiveness,
    pub(crate) attempts: Vec<SignalAttempt>,
    pub(crate) recommended_state: ProcessState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SignalAttempt {
    pub(crate) signal: HostSignal,
    pub(crate) target_process_group_id: u32,
    pub(crate) ok: bool,
    pub(crate) detail: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SignalDelivery {
    pub(crate) ok: bool,
    pub(crate) detail: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostSignal {
    Interrupt,
    Terminate,
    Kill,
}

pub(crate) trait ProcessProbe {
    fn probe(&self, descriptor: &ProcessDescriptor) -> ProcessLiveness;
}

pub(crate) trait ProcessSignaler {
    fn send_group_signal(
        &mut self,
        process_group_id: u32,
        signal: HostSignal,
    ) -> CliResult<SignalDelivery>;
}

pub(crate) trait ProcessSleeper {
    fn sleep(&self, duration: Duration);
}

impl StartedSessionProcess {
    pub(crate) const fn descriptor(&self) -> &ProcessDescriptor {
        &self.descriptor
    }

    pub(crate) fn liveness_from_child(&mut self) -> CliResult<ProcessLiveness> {
        match self.child.try_wait()? {
            Some(status) => Ok(ProcessLiveness {
                status: ProcessLivenessStatus::Exited,
                host_pid: self.descriptor.host_pid,
                host_process_group_id: self.descriptor.host_process_group_id,
                observed_process_group_id: self.descriptor.host_process_group_id,
                message: Some(format!("exit_status={:?}", status.code())),
            }),
            None => Ok(ProcessLiveness {
                status: ProcessLivenessStatus::Running,
                host_pid: self.descriptor.host_pid,
                host_process_group_id: self.descriptor.host_process_group_id,
                observed_process_group_id: self.descriptor.host_process_group_id,
                message: None,
            }),
        }
    }

    pub(crate) fn wait(&mut self) -> CliResult<std::process::ExitStatus> {
        self.child.wait()
    }
}

impl StopMethod {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ProcessGroupInterruptThenKill => "process_group_interrupt_then_kill",
            Self::ProcessGroupTerminateThenKill => "process_group_terminate_then_kill",
        }
    }

    const fn graceful_signal(self) -> HostSignal {
        match self {
            Self::ProcessGroupInterruptThenKill => HostSignal::Interrupt,
            Self::ProcessGroupTerminateThenKill => HostSignal::Terminate,
        }
    }
}

impl std::str::FromStr for StopMethod {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "process_group_interrupt_then_kill" => Ok(Self::ProcessGroupInterruptThenKill),
            "process_group_terminate_then_kill" => Ok(Self::ProcessGroupTerminateThenKill),
            other => Err(format!("unknown stop_method: {other}")),
        }
    }
}

impl HostSignal {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Terminate => "terminate",
            Self::Kill => "kill",
        }
    }

    const fn kill_arg(self) -> &'static str {
        match self {
            Self::Interrupt => "-INT",
            Self::Terminate => "-TERM",
            Self::Kill => "-KILL",
        }
    }
}

impl ProcessLiveness {
    pub(crate) const fn invalid_descriptor(message: String) -> Self {
        Self {
            status: ProcessLivenessStatus::InvalidDescriptor,
            host_pid: None,
            host_process_group_id: None,
            observed_process_group_id: None,
            message: Some(message),
        }
    }

    fn is_running(&self) -> bool {
        self.status == ProcessLivenessStatus::Running
    }
}

pub(crate) fn pre_spawn_descriptor(spec: &SessionProcessSpec) -> ProcessDescriptor {
    ProcessDescriptor {
        name: spec.name.clone(),
        kind: process_kind(spec.kind),
        required: spec.required,
        state: ProcessState::Starting,
        host_pid: None,
        host_process_group_id: None,
        remote_pid: None,
        argv: process_argv(spec),
        remote_command: spec.remote_command.clone(),
        stdout: spec.stdout.clone(),
        stderr: spec.stderr.clone(),
        started_wall_ms: None,
        stop_method: Some(String::from(spec.stop_method.as_str())),
        expected_exit: spec.expected_exit,
        exit_status: None,
        exit_observed_wall_ms: None,
        failure: None,
    }
}

#[cfg(unix)]
pub(crate) fn start_session_process(
    spec: &SessionProcessSpec,
    stdout_path: &Path,
    stderr_path: &Path,
) -> CliResult<StartedSessionProcess> {
    let child =
        spawn_detached_process_to_files(&spec.program, &spec.args, stdout_path, stderr_path)?;
    let child_id = child.child_id();
    let started_wall_ms = host_wall_millis()?;
    let descriptor = ProcessDescriptor {
        state: ProcessState::Running,
        host_pid: Some(child_id),
        host_process_group_id: Some(child_id),
        started_wall_ms: Some(started_wall_ms),
        ..pre_spawn_descriptor(spec)
    };
    Ok(StartedSessionProcess { child, descriptor })
}

#[cfg(not(unix))]
pub(crate) fn start_session_process(
    spec: &SessionProcessSpec,
    stdout_path: &Path,
    stderr_path: &Path,
) -> CliResult<StartedSessionProcess> {
    let _ignore = (spec, stdout_path, stderr_path);
    Err(CliError::new(
        "session process ownership requires a Unix host",
    ))
}

pub(crate) fn probe_descriptor(
    descriptor: &ProcessDescriptor,
    probe: &dyn ProcessProbe,
) -> ProcessLiveness {
    if let Some(invalid) = invalid_descriptor_liveness(descriptor) {
        return invalid;
    }
    probe.probe(descriptor)
}

pub(crate) fn stop_process_group(
    descriptor: &ProcessDescriptor,
    policy: &StopPolicy,
    probe: &dyn ProcessProbe,
    signaler: &mut dyn ProcessSignaler,
) -> StopOutcome {
    stop_process_group_with_sleeper(descriptor, policy, probe, signaler, &ThreadProcessSleeper)
}

fn stop_process_group_with_sleeper(
    descriptor: &ProcessDescriptor,
    policy: &StopPolicy,
    probe: &dyn ProcessProbe,
    signaler: &mut dyn ProcessSignaler,
    sleeper: &dyn ProcessSleeper,
) -> StopOutcome {
    let method = match stop_method_from_descriptor(descriptor) {
        Ok(value) => value,
        Err(message) => {
            return invalid_stop_outcome(
                None,
                ProcessLiveness::invalid_descriptor(message.clone()),
                &message,
            );
        }
    };
    let initial_liveness = probe_descriptor(descriptor, probe);
    if !initial_liveness.is_running() {
        let recommended_state = state_for_non_running_initial(descriptor, initial_liveness.status);
        return StopOutcome {
            ok: false,
            method: Some(method),
            final_liveness: initial_liveness.clone(),
            initial_liveness,
            attempts: Vec::new(),
            recommended_state,
        };
    }
    let Some(process_group_id) = descriptor.host_process_group_id else {
        return invalid_stop_outcome(
            Some(method),
            initial_liveness,
            "missing host_process_group_id",
        );
    };
    let mut attempts = Vec::new();
    let graceful = send_signal_attempt(signaler, process_group_id, method.graceful_signal());
    let graceful_ok = graceful.ok;
    attempts.push(graceful);
    let after_grace = wait_for_not_running(descriptor, probe, policy, sleeper);
    let grace_delivery = if graceful_ok {
        SignalDeliveryState::Delivered
    } else {
        SignalDeliveryState::Failed
    };
    let grace_result = final_stop_result(after_grace.status, grace_delivery);
    if !after_grace.is_running() {
        return StopOutcome {
            ok: grace_result.ok,
            method: Some(method),
            initial_liveness,
            final_liveness: after_grace,
            attempts,
            recommended_state: grace_result.recommended_state,
        };
    }
    let cleanup = send_signal_attempt(signaler, process_group_id, HostSignal::Kill);
    let cleanup_ok = cleanup.ok;
    attempts.push(cleanup);
    let final_liveness = wait_for_not_running(descriptor, probe, policy, sleeper);
    let cleanup_delivery = if cleanup_ok {
        SignalDeliveryState::Delivered
    } else {
        SignalDeliveryState::Failed
    };
    let final_result = final_stop_result(final_liveness.status, cleanup_delivery);
    StopOutcome {
        ok: final_result.ok,
        method: Some(method),
        initial_liveness,
        final_liveness,
        attempts,
        recommended_state: final_result.recommended_state,
    }
}

pub(crate) struct HostProcessProbe;

impl ProcessProbe for HostProcessProbe {
    fn probe(&self, descriptor: &ProcessDescriptor) -> ProcessLiveness {
        host_probe_descriptor(descriptor)
    }
}

pub(crate) struct HostProcessSignaler;

impl ProcessSignaler for HostProcessSignaler {
    fn send_group_signal(
        &mut self,
        process_group_id: u32,
        signal: HostSignal,
    ) -> CliResult<SignalDelivery> {
        send_host_group_signal(process_group_id, signal)
    }
}

pub(crate) struct ThreadProcessSleeper;

impl ProcessSleeper for ThreadProcessSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

const fn process_kind(kind: HostProcessKind) -> ProcessKind {
    match kind {
        HostProcessKind::AdbShell => ProcessKind::AdbShell,
        HostProcessKind::Host => ProcessKind::Host,
    }
}

fn process_argv(spec: &SessionProcessSpec) -> Vec<String> {
    let mut argv = Vec::with_capacity(spec.args.len().saturating_add(1_usize));
    argv.push(spec.program.clone());
    argv.extend(spec.args.iter().cloned());
    argv
}

fn invalid_descriptor_liveness(descriptor: &ProcessDescriptor) -> Option<ProcessLiveness> {
    let Some(host_pid) = descriptor.host_pid else {
        return Some(ProcessLiveness::invalid_descriptor(String::from(
            "missing host_pid",
        )));
    };
    let Some(process_group_id) = descriptor.host_process_group_id else {
        return Some(ProcessLiveness::invalid_descriptor(String::from(
            "missing host_process_group_id",
        )));
    };
    if host_pid == 0_u32 || process_group_id == 0_u32 {
        return Some(ProcessLiveness::invalid_descriptor(String::from(
            "host_pid and host_process_group_id must be positive",
        )));
    }
    if process_group_id == 1_u32 {
        return Some(ProcessLiveness::invalid_descriptor(String::from(
            "refusing to target process group 1",
        )));
    }
    None
}

fn stop_method_from_descriptor(descriptor: &ProcessDescriptor) -> Result<StopMethod, String> {
    let Some(method) = descriptor.stop_method.as_deref() else {
        return Err(String::from("missing stop_method"));
    };
    method.parse::<StopMethod>()
}

fn invalid_stop_outcome(
    method: Option<StopMethod>,
    initial_liveness: ProcessLiveness,
    message: &str,
) -> StopOutcome {
    let final_liveness = ProcessLiveness {
        status: ProcessLivenessStatus::InvalidDescriptor,
        host_pid: initial_liveness.host_pid,
        host_process_group_id: initial_liveness.host_process_group_id,
        observed_process_group_id: initial_liveness.observed_process_group_id,
        message: Some(String::from(message)),
    };
    StopOutcome {
        ok: false,
        method,
        initial_liveness,
        final_liveness,
        attempts: Vec::new(),
        recommended_state: ProcessState::Failed,
    }
}

fn state_for_non_running_initial(
    descriptor: &ProcessDescriptor,
    status: ProcessLivenessStatus,
) -> ProcessState {
    match status {
        ProcessLivenessStatus::Exited if descriptor.expected_exit => ProcessState::Exited,
        ProcessLivenessStatus::Missing if descriptor.state == ProcessState::StopRequested => {
            ProcessState::Stopped
        }
        ProcessLivenessStatus::Missing
        | ProcessLivenessStatus::Exited
        | ProcessLivenessStatus::InvalidDescriptor
        | ProcessLivenessStatus::IdentityMismatch
        | ProcessLivenessStatus::ProbeFailed
        | ProcessLivenessStatus::UnsupportedPlatform
        | ProcessLivenessStatus::Running => ProcessState::Failed,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalStopResult {
    ok: bool,
    recommended_state: ProcessState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignalDeliveryState {
    Delivered,
    Failed,
}

const fn final_stop_result(
    status: ProcessLivenessStatus,
    signal_delivery: SignalDeliveryState,
) -> FinalStopResult {
    match status {
        ProcessLivenessStatus::Missing | ProcessLivenessStatus::Exited
            if matches!(signal_delivery, SignalDeliveryState::Delivered) =>
        {
            FinalStopResult {
                ok: true,
                recommended_state: ProcessState::Stopped,
            }
        }
        ProcessLivenessStatus::Missing
        | ProcessLivenessStatus::Exited
        | ProcessLivenessStatus::Running
        | ProcessLivenessStatus::InvalidDescriptor
        | ProcessLivenessStatus::IdentityMismatch
        | ProcessLivenessStatus::ProbeFailed
        | ProcessLivenessStatus::UnsupportedPlatform => FinalStopResult {
            ok: false,
            recommended_state: ProcessState::Failed,
        },
    }
}

fn send_signal_attempt(
    signaler: &mut dyn ProcessSignaler,
    process_group_id: u32,
    signal: HostSignal,
) -> SignalAttempt {
    match signaler.send_group_signal(process_group_id, signal) {
        Ok(delivery) => SignalAttempt {
            signal,
            target_process_group_id: process_group_id,
            ok: delivery.ok,
            detail: delivery.detail,
        },
        Err(error) => SignalAttempt {
            signal,
            target_process_group_id: process_group_id,
            ok: false,
            detail: json!({
                "error": error.to_string(),
            }),
        },
    }
}

fn wait_for_not_running(
    descriptor: &ProcessDescriptor,
    probe: &dyn ProcessProbe,
    policy: &StopPolicy,
    sleeper: &dyn ProcessSleeper,
) -> ProcessLiveness {
    let poll_count = poll_count(policy);
    let mut index = 0_u32;
    loop {
        let liveness = probe_descriptor(descriptor, probe);
        if !liveness.is_running() {
            return liveness;
        }
        index = index.saturating_add(1_u32);
        if index >= poll_count {
            return liveness;
        }
        sleeper.sleep(policy.poll_interval);
    }
}

fn poll_count(policy: &StopPolicy) -> u32 {
    let interval_ms = policy.poll_interval.as_millis();
    if interval_ms == 0_u128 {
        return 1_u32;
    }
    let timeout_ms = policy.grace_timeout.as_millis();
    let count = timeout_ms
        .checked_div(interval_ms)
        .unwrap_or(0_u128)
        .saturating_add(1_u128);
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn host_wall_millis() -> CliResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::new(format!("host clock is before Unix epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|error| CliError::new(format!("host time overflow: {error}")))
}

fn host_probe_descriptor(descriptor: &ProcessDescriptor) -> ProcessLiveness {
    #[cfg(unix)]
    {
        unix_probe_descriptor(descriptor)
    }
    #[cfg(not(unix))]
    {
        ProcessLiveness {
            status: ProcessLivenessStatus::UnsupportedPlatform,
            host_pid: descriptor.host_pid,
            host_process_group_id: descriptor.host_process_group_id,
            observed_process_group_id: None,
            message: Some(String::from("process groups require a Unix host")),
        }
    }
}

fn send_host_group_signal(process_group_id: u32, signal: HostSignal) -> CliResult<SignalDelivery> {
    #[cfg(unix)]
    {
        let target = format!("-{process_group_id}");
        let args = vec![String::from(signal.kill_arg()), target];
        run_process("kill", &args, FailureMode::AllowFailure).map(|output| SignalDelivery {
            ok: output.status_code == Some(0_i32),
            detail: json!({
                "method": "process_group_signal",
                "signal": signal.as_str(),
                "process_group_id": process_group_id,
                "process": output.json(),
            }),
        })
    }
    #[cfg(not(unix))]
    {
        let _ignore = process_group_id;
        let _ignore_signal = signal;
        Err(CliError::new(
            "process group signaling requires a Unix host",
        ))
    }
}

#[cfg(unix)]
fn unix_probe_descriptor(descriptor: &ProcessDescriptor) -> ProcessLiveness {
    let Some(host_pid) = descriptor.host_pid else {
        return ProcessLiveness::invalid_descriptor(String::from("missing host_pid"));
    };
    let Some(process_group_id) = descriptor.host_process_group_id else {
        return ProcessLiveness::invalid_descriptor(String::from("missing host_process_group_id"));
    };
    match unix_observe_pid(host_pid, process_group_id) {
        Ok(observed) => liveness_for_observed_unix_process(host_pid, process_group_id, &observed),
        Err(liveness) => liveness,
    }
}

#[cfg(unix)]
fn unix_observe_pid(
    host_pid: u32,
    process_group_id: u32,
) -> Result<UnixProcessObservation, ProcessLiveness> {
    let output = run_process(
        "ps",
        &[
            String::from("-o"),
            String::from("pgid="),
            String::from("-o"),
            String::from("stat="),
            String::from("-p"),
            host_pid.to_string(),
        ],
        FailureMode::AllowFailure,
    )
    .map_err(|error| ProcessLiveness {
        status: ProcessLivenessStatus::ProbeFailed,
        host_pid: Some(host_pid),
        host_process_group_id: Some(process_group_id),
        observed_process_group_id: None,
        message: Some(error.to_string()),
    })?;
    if output.status_code != Some(0_i32) || output.stdout().trim().is_empty() {
        return Err(liveness_for_missing_pid_with_group_probe(
            host_pid,
            process_group_id,
        ));
    }
    parse_unix_process_observation(output.stdout()).map_err(|message| ProcessLiveness {
        status: ProcessLivenessStatus::ProbeFailed,
        host_pid: Some(host_pid),
        host_process_group_id: Some(process_group_id),
        observed_process_group_id: None,
        message: Some(message),
    })
}

#[cfg(unix)]
fn liveness_for_observed_unix_process(
    host_pid: u32,
    process_group_id: u32,
    observed: &UnixProcessObservation,
) -> ProcessLiveness {
    if observed.process_group_id != process_group_id {
        return ProcessLiveness {
            status: ProcessLivenessStatus::IdentityMismatch,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(observed.process_group_id),
            message: Some(String::from(
                "host pid belongs to a different process group",
            )),
        };
    }
    match unix_process_group_status(process_group_id) {
        Ok(status) => liveness_for_observed_unix_process_group_status(
            host_pid,
            process_group_id,
            observed,
            status,
        ),
        Err(error) => ProcessLiveness {
            status: ProcessLivenessStatus::ProbeFailed,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: None,
            message: Some(error),
        },
    }
}

#[cfg(unix)]
fn liveness_for_observed_unix_process_group_status(
    host_pid: u32,
    process_group_id: u32,
    observed: &UnixProcessObservation,
    group_status: UnixProcessGroupStatus,
) -> ProcessLiveness {
    match (observed.is_zombie, group_status) {
        (true, UnixProcessGroupStatus::Exists) => ProcessLiveness {
            status: ProcessLivenessStatus::Running,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(observed.process_group_id),
            message: Some(String::from(
                "host pid is a zombie but process group still exists",
            )),
        },
        (true, UnixProcessGroupStatus::Missing) => ProcessLiveness {
            status: ProcessLivenessStatus::Exited,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(observed.process_group_id),
            message: Some(String::from(
                "host pid is a zombie and process group is gone",
            )),
        },
        (false, UnixProcessGroupStatus::Exists) => ProcessLiveness {
            status: ProcessLivenessStatus::Running,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(observed.process_group_id),
            message: None,
        },
        (false, UnixProcessGroupStatus::Missing) => ProcessLiveness {
            status: ProcessLivenessStatus::ProbeFailed,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(observed.process_group_id),
            message: Some(String::from(
                "host pid exists but process group probe did not find the group",
            )),
        },
    }
}

#[cfg(unix)]
fn liveness_for_missing_pid_with_group_probe(
    host_pid: u32,
    process_group_id: u32,
) -> ProcessLiveness {
    match unix_process_group_status(process_group_id) {
        Ok(UnixProcessGroupStatus::Missing) => ProcessLiveness {
            status: ProcessLivenessStatus::Missing,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: None,
            message: Some(String::from("host pid and process group were not found")),
        },
        Ok(UnixProcessGroupStatus::Exists) => ProcessLiveness {
            status: ProcessLivenessStatus::IdentityMismatch,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: Some(process_group_id),
            message: Some(String::from(
                "host pid was not found but process group still exists",
            )),
        },
        Err(message) => ProcessLiveness {
            status: ProcessLivenessStatus::ProbeFailed,
            host_pid: Some(host_pid),
            host_process_group_id: Some(process_group_id),
            observed_process_group_id: None,
            message: Some(message),
        },
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnixProcessGroupStatus {
    Exists,
    Missing,
}

#[cfg(unix)]
fn unix_process_group_status(process_group_id: u32) -> Result<UnixProcessGroupStatus, String> {
    let target = format!("-{process_group_id}");
    let args = vec![String::from("-0"), target];
    let output = run_process("kill", &args, FailureMode::AllowFailure)
        .map_err(|error| format!("failed to probe process group: {error}"))?;
    if output.status_code == Some(0_i32) {
        Ok(UnixProcessGroupStatus::Exists)
    } else {
        Ok(UnixProcessGroupStatus::Missing)
    }
}

#[cfg(unix)]
struct UnixProcessObservation {
    process_group_id: u32,
    is_zombie: bool,
}

#[cfg(unix)]
fn parse_unix_process_observation(stdout: &str) -> Result<UnixProcessObservation, String> {
    let mut words = stdout.split_whitespace();
    let Some(group_text) = words.next() else {
        return Err(String::from("missing process group id"));
    };
    let process_group_id = group_text
        .parse::<u32>()
        .map_err(|error| format!("failed to parse process group id: {error}"))?;
    let status_text = words.next().unwrap_or("");
    Ok(UnixProcessObservation {
        process_group_id,
        is_zombie: status_text.contains('Z'),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use crate::error::CliResult;
    use crate::session_process::{
        HostProcessKind, HostProcessProbe, HostSignal, ProcessLiveness, ProcessLivenessStatus,
        ProcessProbe, ProcessSignaler, ProcessSleeper, SessionProcessSpec, SignalDelivery,
        StopMethod, StopPolicy, pre_spawn_descriptor, probe_descriptor, start_session_process,
        stop_process_group, stop_process_group_with_sleeper,
    };
    use crate::session_state::schema::{ProcessDescriptor, ProcessKind, ProcessState};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0_u64);

    #[test]
    fn process_liveness_status_serializes_stable_snake_case() {
        let values = [
            (ProcessLivenessStatus::Running, json!("running")),
            (ProcessLivenessStatus::Exited, json!("exited")),
            (ProcessLivenessStatus::Missing, json!("missing")),
            (
                ProcessLivenessStatus::InvalidDescriptor,
                json!("invalid_descriptor"),
            ),
            (
                ProcessLivenessStatus::IdentityMismatch,
                json!("identity_mismatch"),
            ),
            (ProcessLivenessStatus::ProbeFailed, json!("probe_failed")),
            (
                ProcessLivenessStatus::UnsupportedPlatform,
                json!("unsupported_platform"),
            ),
        ];

        for (status, expected) in values {
            assert_eq!(
                serde_json::to_value(status).ok(),
                Some(expected),
                "process liveness status should serialize stably"
            );
        }
    }

    #[test]
    fn stop_method_vocabulary_round_trips() {
        let values = [
            (
                StopMethod::ProcessGroupInterruptThenKill,
                "process_group_interrupt_then_kill",
            ),
            (
                StopMethod::ProcessGroupTerminateThenKill,
                "process_group_terminate_then_kill",
            ),
        ];

        for (method, text) in values {
            assert_eq!(method.as_str(), text);
            assert_eq!(text.parse::<StopMethod>().ok(), Some(method));
        }
        assert!("process_group_unknown".parse::<StopMethod>().is_err());
    }

    #[test]
    fn pre_spawn_descriptor_records_starting_contract_without_pid() {
        let spec = synthetic_spec("screenrecord");
        let descriptor = pre_spawn_descriptor(&spec);

        assert_eq!(descriptor.name, "screenrecord");
        assert_eq!(descriptor.kind, ProcessKind::AdbShell);
        assert_eq!(descriptor.state, ProcessState::Starting);
        assert_eq!(descriptor.host_pid, None);
        assert_eq!(descriptor.host_process_group_id, None);
        assert_eq!(
            descriptor.stop_method.as_deref(),
            Some("process_group_interrupt_then_kill")
        );
        assert_eq!(
            descriptor.argv,
            vec![
                String::from("adb"),
                String::from("shell"),
                String::from("screenrecord")
            ]
        );
    }

    #[test]
    fn invalid_descriptor_does_not_probe() {
        let descriptor = ProcessDescriptor {
            host_pid: None,
            ..synthetic_descriptor(ProcessState::Running)
        };
        let probe = CountingProbe::new(vec![running_liveness(9_u32)]);
        let liveness = probe_descriptor(&descriptor, &probe);

        assert_eq!(liveness.status, ProcessLivenessStatus::InvalidDescriptor);
        assert_eq!(probe.call_count.get(), 0_u64);
    }

    #[test]
    fn stop_missing_process_is_not_false_success() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![missing_liveness(7_u32), missing_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok, "missing required process should not be ok");
        assert_eq!(
            outcome.method,
            Some(StopMethod::ProcessGroupTerminateThenKill)
        );
        assert_eq!(
            outcome.initial_liveness.status,
            ProcessLivenessStatus::Missing
        );
        assert_eq!(outcome.recommended_state, ProcessState::Failed);
        assert!(
            signaler.signals.borrow().is_empty(),
            "missing process should not be signaled"
        );
    }

    #[test]
    fn stop_identity_mismatch_does_not_signal() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![ProcessLiveness {
            status: ProcessLivenessStatus::IdentityMismatch,
            host_pid: Some(7_u32),
            host_process_group_id: Some(7_u32),
            observed_process_group_id: Some(99_u32),
            message: Some(String::from("mismatch")),
        }]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok, "identity mismatch should fail closed");
        assert_eq!(
            outcome.initial_liveness.status,
            ProcessLivenessStatus::IdentityMismatch
        );
        assert!(
            signaler.signals.borrow().is_empty(),
            "identity mismatch should not signal"
        );
    }

    #[test]
    fn stop_escalates_when_process_remains_running() {
        let mut descriptor = synthetic_descriptor(ProcessState::Running);
        descriptor.stop_method = Some(String::from("process_group_interrupt_then_kill"));
        let probe = CountingProbe::new(vec![
            running_liveness(7_u32),
            running_liveness(7_u32),
            running_liveness(7_u32),
        ]);
        let mut signaler = RecordingSignaler::default();
        let sleeper = RecordingSleeper::default();
        let outcome = stop_process_group_with_sleeper(
            &descriptor,
            &zero_policy(),
            &probe,
            &mut signaler,
            &sleeper,
        );

        assert!(
            !outcome.ok,
            "still-running process should fail after escalation"
        );
        assert_eq!(
            signaler.signals.borrow().as_slice(),
            &[(7_u32, HostSignal::Interrupt), (7_u32, HostSignal::Kill)],
            "stop should signal the process group in order"
        );
        assert_eq!(outcome.recommended_state, ProcessState::Failed);
        assert!(
            sleeper.sleeps.borrow().is_empty(),
            "zero policy should not sleep"
        );
    }

    #[test]
    fn stop_uses_group_signal_and_recommends_stopped_after_missing() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![running_liveness(7_u32), missing_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(outcome.ok, "missing after signal should be stopped");
        assert_eq!(
            outcome.final_liveness.status,
            ProcessLivenessStatus::Missing
        );
        assert_eq!(
            signaler.signals.borrow().as_slice(),
            &[(7_u32, HostSignal::Terminate)]
        );
        assert_eq!(outcome.recommended_state, ProcessState::Stopped);
    }

    #[test]
    fn stop_rejects_missing_stop_method_before_probe_or_signal() {
        let descriptor = ProcessDescriptor {
            stop_method: None,
            ..synthetic_descriptor(ProcessState::Running)
        };
        let probe = CountingProbe::new(vec![running_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok);
        assert_eq!(outcome.method, None);
        assert_eq!(
            outcome.initial_liveness.status,
            ProcessLivenessStatus::InvalidDescriptor
        );
        assert_eq!(probe.call_count.get(), 0_u64);
        assert!(signaler.signals.borrow().is_empty());
    }

    #[test]
    fn stop_rejects_unknown_stop_method_before_probe_or_signal() {
        let descriptor = ProcessDescriptor {
            stop_method: Some(String::from("unknown_stop_method")),
            ..synthetic_descriptor(ProcessState::Running)
        };
        let probe = CountingProbe::new(vec![running_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok);
        assert_eq!(outcome.method, None);
        assert_eq!(
            outcome.initial_liveness.status,
            ProcessLivenessStatus::InvalidDescriptor
        );
        assert_eq!(probe.call_count.get(), 0_u64);
        assert!(signaler.signals.borrow().is_empty());
    }

    #[test]
    fn stop_exited_required_process_is_failed_without_signal() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![exited_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok);
        assert_eq!(
            outcome.initial_liveness.status,
            ProcessLivenessStatus::Exited
        );
        assert_eq!(outcome.recommended_state, ProcessState::Failed);
        assert!(signaler.signals.borrow().is_empty());
    }

    #[test]
    fn stop_expected_exit_process_recommends_exited_without_signal() {
        let descriptor = ProcessDescriptor {
            expected_exit: true,
            ..synthetic_descriptor(ProcessState::Running)
        };
        let probe = CountingProbe::new(vec![exited_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::default();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(!outcome.ok);
        assert_eq!(outcome.recommended_state, ProcessState::Exited);
        assert!(signaler.signals.borrow().is_empty());
    }

    #[test]
    fn failed_signal_delivery_prevents_false_success() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![running_liveness(7_u32), missing_liveness(7_u32)]);
        let mut signaler = RecordingSignaler::failing_delivery();
        let outcome = stop_process_group(&descriptor, &zero_policy(), &probe, &mut signaler);

        assert!(
            !outcome.ok,
            "failed signal delivery should not report a clean stop"
        );
        assert_eq!(outcome.recommended_state, ProcessState::Failed);
        assert_eq!(
            outcome.attempts.first().map(|attempt| attempt.ok),
            Some(false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn zombie_leader_with_live_group_stays_running_for_cleanup() {
        let observed = crate::session_process::UnixProcessObservation {
            process_group_id: 7_u32,
            is_zombie: true,
        };
        let liveness = crate::session_process::liveness_for_observed_unix_process_group_status(
            7_u32,
            7_u32,
            &observed,
            crate::session_process::UnixProcessGroupStatus::Exists,
        );

        assert_eq!(liveness.status, ProcessLivenessStatus::Running);
    }

    #[cfg(unix)]
    #[test]
    fn zombie_leader_with_missing_group_is_exited() {
        let observed = crate::session_process::UnixProcessObservation {
            process_group_id: 7_u32,
            is_zombie: true,
        };
        let liveness = crate::session_process::liveness_for_observed_unix_process_group_status(
            7_u32,
            7_u32,
            &observed,
            crate::session_process::UnixProcessGroupStatus::Missing,
        );

        assert_eq!(liveness.status, ProcessLivenessStatus::Exited);
    }

    #[test]
    fn stop_polling_uses_injected_sleeper_with_exact_probe_count() {
        let descriptor = synthetic_descriptor(ProcessState::Running);
        let probe = CountingProbe::new(vec![
            running_liveness(7_u32),
            running_liveness(7_u32),
            running_liveness(7_u32),
            missing_liveness(7_u32),
        ]);
        let mut signaler = RecordingSignaler::default();
        let sleeper = RecordingSleeper::default();
        let policy = StopPolicy {
            grace_timeout: Duration::from_millis(2),
            poll_interval: Duration::from_millis(1),
        };
        let outcome =
            stop_process_group_with_sleeper(&descriptor, &policy, &probe, &mut signaler, &sleeper);

        assert!(outcome.ok);
        assert_eq!(probe.call_count.get(), 4_u64);
        assert_eq!(
            sleeper.sleeps.borrow().as_slice(),
            &[Duration::from_millis(1), Duration::from_millis(1)]
        );
    }

    #[test]
    fn helper_does_not_create_session_state_or_lock_files() {
        let root = unique_temp_dir("session-process-no-session-files");
        let stdout = root.join("stdout.log");
        let stderr = root.join("stderr.log");
        let spec = SessionProcessSpec {
            stdout: path_string(&stdout),
            stderr: path_string(&stderr),
            ..synthetic_spec("host-sleep")
        };
        let descriptor = pre_spawn_descriptor(&spec);

        assert_eq!(descriptor.stdout, path_string(&stdout));
        assert_eq!(descriptor.stderr, path_string(&stderr));
        assert!(
            !root.join("session").exists(),
            "descriptor construction should not create session dir"
        );
        assert!(
            !root.join("capture-session.lock.json").exists(),
            "descriptor construction should not create lock file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn start_records_running_descriptor_and_requested_logs() {
        let root = unique_temp_dir("session-process-start");
        let stdout = root.join("stdout.log");
        let stderr = root.join("stderr.log");
        let mut spec = synthetic_unix_sleep_spec(&stdout, &stderr);
        spec.args = vec![
            String::from("-c"),
            String::from("echo out; echo err >&2; sleep 5"),
        ];
        let Some(mut process) = assert_ok(
            start_session_process(&spec, &stdout, &stderr),
            "start process",
        ) else {
            return;
        };
        let descriptor = process.descriptor().clone();
        let liveness_result = process.liveness_from_child();
        cleanup_process(&descriptor);
        let _wait = process.wait();
        let Some(liveness) = assert_ok(liveness_result, "child liveness") else {
            return;
        };

        assert_eq!(descriptor.state, ProcessState::Running);
        assert_eq!(descriptor.host_pid, descriptor.host_process_group_id);
        assert_eq!(descriptor.stdout, path_string(&stdout));
        assert_eq!(descriptor.stderr, path_string(&stderr));
        assert_eq!(liveness.status, ProcessLivenessStatus::Running);
        assert!(
            stdout.exists() && stderr.exists(),
            "requested log files should be created"
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_probe_reports_running_for_started_process() {
        let root = unique_temp_dir("session-process-probe");
        let stdout = root.join("stdout.log");
        let stderr = root.join("stderr.log");
        let spec = synthetic_unix_sleep_spec(&stdout, &stderr);
        let Some(mut process) = assert_ok(
            start_session_process(&spec, &stdout, &stderr),
            "start process",
        ) else {
            return;
        };
        let descriptor = process.descriptor().clone();
        let probe = HostProcessProbe;
        let liveness = probe_descriptor(&descriptor, &probe);
        cleanup_process(&descriptor);
        let _wait = process.wait();

        assert_eq!(liveness.status, ProcessLivenessStatus::Running);
        assert_eq!(
            liveness.observed_process_group_id,
            descriptor.host_process_group_id
        );
    }

    #[cfg(unix)]
    #[test]
    fn host_group_stop_terminates_started_process() {
        let root = unique_temp_dir("session-process-stop");
        let stdout = root.join("stdout.log");
        let stderr = root.join("stderr.log");
        let spec = synthetic_unix_sleep_spec(&stdout, &stderr);
        let Some(mut process) = assert_ok(
            start_session_process(&spec, &stdout, &stderr),
            "start process",
        ) else {
            return;
        };
        let descriptor = process.descriptor().clone();
        let probe = HostProcessProbe;
        let mut signaler = crate::session_process::HostProcessSignaler;
        let outcome = stop_process_group(&descriptor, &short_policy(), &probe, &mut signaler);
        let _wait = process.wait();

        assert!(outcome.ok, "group stop should terminate fake process");
        assert_eq!(outcome.recommended_state, ProcessState::Stopped);
    }

    fn synthetic_spec(name: &str) -> SessionProcessSpec {
        SessionProcessSpec {
            name: String::from(name),
            kind: HostProcessKind::AdbShell,
            required: true,
            program: String::from("adb"),
            args: vec![String::from("shell"), String::from("screenrecord")],
            remote_command: vec![String::from("screenrecord")],
            stdout: String::from("video/stdout.log"),
            stderr: String::from("video/stderr.log"),
            stop_method: StopMethod::ProcessGroupInterruptThenKill,
            expected_exit: false,
        }
    }

    fn synthetic_descriptor(state: ProcessState) -> ProcessDescriptor {
        ProcessDescriptor {
            name: String::from("getevent"),
            kind: ProcessKind::AdbShell,
            required: true,
            state,
            host_pid: Some(7_u32),
            host_process_group_id: Some(7_u32),
            remote_pid: None,
            argv: vec![String::from("adb"), String::from("shell")],
            remote_command: vec![String::from("getevent")],
            stdout: String::from("adb/getevent.raw.log"),
            stderr: String::from("adb/getevent.stderr.log"),
            started_wall_ms: Some(1_u64),
            stop_method: Some(String::from("process_group_terminate_then_kill")),
            expected_exit: false,
            exit_status: None,
            exit_observed_wall_ms: None,
            failure: None,
        }
    }

    fn running_liveness(pid: u32) -> ProcessLiveness {
        ProcessLiveness {
            status: ProcessLivenessStatus::Running,
            host_pid: Some(pid),
            host_process_group_id: Some(pid),
            observed_process_group_id: Some(pid),
            message: None,
        }
    }

    fn missing_liveness(pid: u32) -> ProcessLiveness {
        ProcessLiveness {
            status: ProcessLivenessStatus::Missing,
            host_pid: Some(pid),
            host_process_group_id: Some(pid),
            observed_process_group_id: None,
            message: Some(String::from("missing")),
        }
    }

    fn exited_liveness(pid: u32) -> ProcessLiveness {
        ProcessLiveness {
            status: ProcessLivenessStatus::Exited,
            host_pid: Some(pid),
            host_process_group_id: Some(pid),
            observed_process_group_id: Some(pid),
            message: Some(String::from("exited")),
        }
    }

    fn zero_policy() -> StopPolicy {
        StopPolicy {
            grace_timeout: Duration::from_millis(0),
            poll_interval: Duration::from_millis(1),
        }
    }

    fn short_policy() -> StopPolicy {
        StopPolicy {
            grace_timeout: Duration::from_millis(500),
            poll_interval: Duration::from_millis(20),
        }
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let counter = TEMP_COUNTER.fetch_add(1_u64, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "input-dynamics-{label}-{}-{counter}",
            std::process::id()
        ));
        let _cleanup = fs::remove_dir_all(&root);
        if let Err(error) = fs::create_dir_all(&root) {
            return std::env::temp_dir().join(format!(
                "input-dynamics-{label}-fallback-{}-{counter}-{error}",
                std::process::id()
            ));
        }
        root
    }

    fn path_string(path: &std::path::Path) -> String {
        path.to_string_lossy().to_string()
    }

    #[cfg(unix)]
    fn synthetic_unix_sleep_spec(
        stdout: &std::path::Path,
        stderr: &std::path::Path,
    ) -> SessionProcessSpec {
        SessionProcessSpec {
            name: String::from("host-sleep"),
            kind: HostProcessKind::Host,
            required: true,
            program: String::from("sh"),
            args: vec![String::from("-c"), String::from("sleep 30")],
            remote_command: Vec::new(),
            stdout: path_string(stdout),
            stderr: path_string(stderr),
            stop_method: StopMethod::ProcessGroupTerminateThenKill,
            expected_exit: false,
        }
    }

    #[cfg(unix)]
    fn cleanup_process(descriptor: &ProcessDescriptor) {
        let Some(group_id) = descriptor.host_process_group_id else {
            return;
        };
        let mut signaler = crate::session_process::HostProcessSignaler;
        let _signal = signaler.send_group_signal(group_id, HostSignal::Kill);
    }

    struct CountingProbe {
        results: RefCell<Vec<ProcessLiveness>>,
        call_count: Cell<u64>,
    }

    impl CountingProbe {
        fn new(mut results: Vec<ProcessLiveness>) -> Self {
            results.reverse();
            Self {
                results: RefCell::new(results),
                call_count: Cell::new(0_u64),
            }
        }
    }

    impl ProcessProbe for CountingProbe {
        fn probe(&self, _descriptor: &ProcessDescriptor) -> ProcessLiveness {
            self.call_count
                .set(self.call_count.get().saturating_add(1_u64));
            self.results
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| running_liveness(7_u32))
        }
    }

    struct RecordingSignaler {
        signals: RefCell<SignalRecords>,
        delivery_ok: Cell<bool>,
    }

    impl Default for RecordingSignaler {
        fn default() -> Self {
            Self {
                signals: RefCell::new(Vec::new()),
                delivery_ok: Cell::new(true),
            }
        }
    }

    impl RecordingSignaler {
        fn failing_delivery() -> Self {
            Self {
                signals: RefCell::new(Vec::new()),
                delivery_ok: Cell::new(false),
            }
        }
    }

    type SignalRecord = (u32, HostSignal);
    type SignalRecords = Vec<SignalRecord>;

    impl ProcessSignaler for RecordingSignaler {
        fn send_group_signal(
            &mut self,
            process_group_id: u32,
            signal: HostSignal,
        ) -> CliResult<SignalDelivery> {
            self.signals.borrow_mut().push((process_group_id, signal));
            Ok(SignalDelivery {
                ok: self.delivery_ok.get(),
                detail: json!({
                    "signal": signal.as_str(),
                    "process_group_id": process_group_id,
                }),
            })
        }
    }

    #[derive(Default)]
    struct RecordingSleeper {
        sleeps: RefCell<Vec<Duration>>,
    }

    impl ProcessSleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.sleeps.borrow_mut().push(duration);
        }
    }

    fn assert_ok<T, E>(result: Result<T, E>, label: &str) -> Option<T>
    where
        E: std::fmt::Debug,
    {
        let error = result.as_ref().err();
        assert!(error.is_none(), "{label} failed: {error:?}");
        result.ok()
    }
}
