// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart start` - RESTART-TOOL-DESIGN.md 14.5 and 14.8: start a lane that is not
//! running, from its launch spec (`<role>.launch.json`), and write the spec back as `running`
//! once the session is confirmed live.
//!
//! The decision is separate from the effect so the refusals are testable without a window:
//! `plan_start` reads the disk and the process table and either refuses or returns the exact
//! launch; `execute_start` spawns it and waits.
//!
//! Three rules that are the point of the module:
//! * **PM only, by process, not by claim** (`caller_role`): the caller's `claude` ancestor must
//!   be the recorded pid and start time of `pm`'s state file. `LANE_ROLE` is never consulted.
//! * **Never Opus** (CireSnave, 2026-10-08: "I can't afford Opus."): the model is `sonnet` or
//!   `haiku`; `LANE_RESTART_ALLOW_OPUS` is deliberately not read on this path.
//! * **Start only a lane that is not running**, and recognise the NEW session as the one that
//!   is not the dead session recorded in the lane's state file (which is stale, not new).

use crate::duplicate::{self, DuplicateRefusal};
use crate::facts::SystemFacts;
use crate::launch::{launch_and_wait_with, prepare_launch, LaunchSpec, LivenessTiming, SpawnFn};
use crate::relaunch::{valid_identifier, RelaunchError, RelaunchOutcome, StateReader};
use crate::spec::{self, role_status, Lifecycle, SpecError, SpecFile, SpecState, SpecStore};
use crate::state;
use std::path::Path;
use std::time::Duration;

/// The longest `--reason`, in characters (14.5).
pub const REASON_MAX_CHARS: usize = 200;

/// The permission mode every started lane gets (the PM's answer to O1, 2026-10-11).
pub const START_PERMISSION_MODE: &str = "auto";

/// The role that a start may not target (the PM is started by a person; O1).
pub const PM_ROLE: &str = "pm";

/// The one non-PM role that may call `start`, and only `start` (CireSnave 2026-10-15,
/// "Synapse->OverMind->AgentLife launch", relayed by the PM): the launcher service that acts on a
/// Synapse mail-waiting signal. `stop` and a restart never accept it (`lane_stop::require_pm_for_other`
/// and `authorize::decide` read their own caller).
pub const LAUNCHER_ROLE: &str = "agentlife-launcher";

/// The name 0.15.2 used. Accepted for one more release so a record written by an earlier service
/// build still works; the PM ruling (2026-10-15) is that the service never claims `overmind`, since
/// one Synapse session per role would knock the OverMind lane off it. Remove after 0.16.
pub const LEGACY_LAUNCHER_ROLE: &str = "overmind";

/// Is `role` a name the launcher service may act under?
pub fn is_launcher_role(role: &str) -> bool {
    role == LAUNCHER_ROLE || role == LEGACY_LAUNCHER_ROLE
}

/// What `launcher_caller` returns, followed by the role from the record: `launcher:<role>`. A `:`
/// cannot occur in a lane's role (state files are `<identifier>.json`), so a *lane* that merely
/// shares a name with the launcher (the OverMind lane's role is `overmind`) can never present
/// this: only a validated launcher record produces it.
pub const LAUNCHER_CALLER_PREFIX: &str = "launcher:";

/// Is `caller` the output of a validated launcher record?
pub fn is_launcher_caller(caller: &str) -> bool {
    caller
        .strip_prefix(LAUNCHER_CALLER_PREFIX)
        .is_some_and(is_launcher_role)
}

/// Where the launcher service mirrors its identity. Deliberately not `*.json`: every other reader of
/// the state dir treats `<stem>.json` as a lane's state file. One file per accepted role name.
pub fn launcher_record_name(role: &str) -> String {
    format!("{role}.launcher-caller")
}

/// The 0.15.2 record name, kept so code that named it still compiles. Use [`launcher_record_name`].
#[deprecated(
    note = "use launcher_record_name(LAUNCHER_ROLE); the legacy name is LEGACY_LAUNCHER_ROLE"
)]
pub const LAUNCHER_RECORD: &str = "overmind.launcher-caller";

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Eq)]
pub struct LauncherRecord {
    pub role: String,
    pub pid: u32,
    pub pid_start_secs: u64,
}

/// The launcher service, recognised by its own record: `parent` (the process that ran this
/// `lane-restart start`, one level up and no further) must be the recorded pid AND start time, so
/// a reused pid does not inherit it. The record is keyed by role, not by a `claude` ancestor,
/// because the service is not a Claude session.
///
/// Limit, stated: anything running as the same user can write this file and so forge the identity
/// (like `caller_role`'s state files). What the gate bounds is *what that identity may do*; see
/// `docs/LAUNCH-ON-MESSAGE-THREAT.md`.
pub fn launcher_caller(
    dir: &Path,
    parent: Option<u32>,
    facts: &dyn SystemFacts,
) -> Result<String, CallerError> {
    let pid = parent.ok_or(CallerError::NoClaudeAncestor)?;
    for role in [LAUNCHER_ROLE, LEGACY_LAUNCHER_ROLE] {
        let Ok(bytes) = std::fs::read(dir.join(launcher_record_name(role))) else {
            continue;
        };
        let Ok(rec) = serde_json::from_slice::<LauncherRecord>(&bytes) else {
            continue;
        };
        let live = facts
            .process_identity(pid)
            .is_some_and(|id| id.start_time_secs == rec.pid_start_secs);
        // The file's name and the role inside it must agree, so one record cannot stand in for
        // the other name.
        if rec.role == role && rec.pid == pid && live {
            return Ok(format!("{LAUNCHER_CALLER_PREFIX}{role}"));
        }
    }
    Err(CallerError::Unrecognised(pid))
}

/// A Synapse signal id is `<role>:<oldest queued global id>`. For a launcher call it must name the
/// role being started, so a signal for one lane cannot be spent on another.
pub fn signal_names_role(signal_id: &str, role: &str) -> bool {
    let ok_chars = signal_id.len() <= 128
        && signal_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
    ok_chars
        && signal_id
            .strip_prefix(role)
            .and_then(|r| r.strip_prefix(':'))
            .is_some_and(|rest| !rest.is_empty())
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallerError {
    /// No `claude` ancestor could be established for this process.
    NoClaudeAncestor,
    /// The ancestor is a live `claude`, but no lane state file records that pid and start time.
    Unrecognised(u32),
    /// More than one state file claims the pid: refuse rather than choose.
    Ambiguous(u32),
}

impl std::fmt::Display for CallerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallerError::NoClaudeAncestor => {
                write!(f, "this command is not running under a claude session")
            }
            CallerError::Unrecognised(pid) => write!(
                f,
                "claude pid {pid} is not recorded by any lane state file (pid and start time)"
            ),
            CallerError::Ambiguous(pid) => {
                write!(f, "more than one lane state file records claude pid {pid}")
            }
        }
    }
}

/// The role whose session is running this command: the lane state file whose recorded pid AND
/// start time equal the caller's `claude` ancestor (14.8). Pid-based, never environment-based,
/// so a lane cannot claim to be the PM by setting `LANE_ROLE` in a child process.
///
/// Limit, stated: a lane that edits `.lane-state/pm.json` can forge this (same user, unsigned
/// files). It closes the accidental and the prompt-injected path, not a hostile local user.
pub fn caller_role(
    dir: &Path,
    claude_pid: Option<u32>,
    facts: &dyn SystemFacts,
) -> Result<String, CallerError> {
    let pid = claude_pid.ok_or(CallerError::NoClaudeAncestor)?;
    let mut found: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if spec::is_spec_file_name(&name) {
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if !valid_identifier(stem) {
                continue;
            }
            let Ok(st) = state::load(dir, stem) else {
                continue;
            };
            if st.role == stem && st.pid == pid && spec::state_process_is_live(&st, facts) {
                found.push(stem.to_string());
            }
        }
    }
    match found.len() {
        0 => Err(CallerError::Unrecognised(pid)),
        1 => Ok(found.remove(0)),
        _ => Err(CallerError::Ambiguous(pid)),
    }
}

/// `--reason`: non-empty after trimming, at most 200 characters, no control characters (14.5).
/// A launch costs tokens and must name its work.
pub fn validate_reason(reason: &str) -> Result<String, &'static str> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err("--reason is required and must not be empty");
    }
    if trimmed.chars().count() > REASON_MAX_CHARS {
        return Err("--reason is longer than 200 characters");
    }
    if trimmed.chars().any(char::is_control) {
        return Err("--reason must not contain control characters");
    }
    Ok(trimmed.to_string())
}

/// `--model`: only `sonnet` or `haiku` may be requested (14.5).
pub fn validate_model_request(model: &str) -> Result<&'static str, String> {
    match model {
        "sonnet" => Ok("sonnet"),
        "haiku" => Ok("haiku"),
        other => Err(other.to_string()),
    }
}

pub struct StartRequest<'a> {
    pub role: &'a str,
    pub reason: &'a str,
    /// `--model`, as typed.
    pub model: Option<&'a str>,
    /// `--signal-id`: the Synapse mail-waiting signal this start answers. Required of the
    /// launcher service, optional (and ledgered when given) for the PM.
    pub signal_id: Option<&'a str>,
}

#[derive(Debug)]
pub enum StartRefusal {
    /// The caller is not the PM session; carries what was established about it.
    NotPm(Result<String, CallerError>),
    /// A start of role `pm` is refused-and-reported: the PM is started by a person (O1).
    PmRole,
    /// The launcher service called without a signal id that names the role (see
    /// `signal_names_role`): it may start a lane only because mail waits for it.
    NoSignal,
    InvalidRole(String),
    Reason(&'static str),
    /// A model other than `sonnet` or `haiku` was requested; an Opus request lands here too.
    Model(String),
    /// No spec, so no way to know how to launch this lane: never guessed.
    NoSpec(String),
    SpecUnusable(String),
    AlreadyRunning,
    /// A `lane-restart host --role R` or a claude named for R is in the process table although
    /// no live recorded session is (the state file is stale, or its session never started).
    DuplicateRisk(DuplicateRefusal),
    /// Another starter of this role holds `<role>.start.lock`: the role is being started right now.
    /// Same contract as `AlreadyRunning` (a no-op, exit 3).
    StartLockHeld,
    /// The start lock could not be created or taken; nothing was launched.
    StartLockUnavailable(String),
    /// The launcher may start only a lane that ended on its own (board 216): this spec records a
    /// stop that a person or the PM made on purpose (`stop-hard`) or none (`stop-unclassified`).
    /// Terminal: asking again changes nothing until a person or the PM starts the lane.
    StopNotSoft(&'static str),
    /// A launcher start reached a cap (0.15.13): `refused`, terminal until a slot frees.
    RateCap(crate::launcher_gate::CapHit),
    /// Free memory is below the floor `agentlife restore` holds (0.15.13): `failed`, may succeed later.
    HeldForMemory {
        free_gb: f64,
        floor_gb: f64,
    },
    /// `restore.lock` is held by a live `agentlife restore` or another launcher start (0.15.13).
    RestoreLockHeld {
        pid: u32,
    },
    /// A launcher gate could not be read, so nothing was launched (fail closed, 0.15.13): the
    /// ledger, the floor's config, or the lock file. `reason` is the vocabulary-A code.
    GateUnavailable {
        reason: &'static str,
        why: String,
    },
    Launch(RelaunchError),
}

/// Exit code of a start that was refused because the lane is already running: a no-op, not a
/// failure, so a caller that wants "make sure it is up" (OverMind, on mail for a stopped lane)
/// can tell it from every other refusal (exit 1). 0 and 2 are a launch; 4 is `stop`'s.
pub const EXIT_ALREADY_RUNNING: u8 = 3;

impl StartRefusal {
    /// The process exit code for this refusal: `EXIT_ALREADY_RUNNING` for `AlreadyRunning`, 1
    /// for every other refusal. A `duplicate-risk` refusal stays 1: a stuck or unrecorded
    /// session is not a confirmed live lane.
    pub fn exit_code(&self) -> u8 {
        match self {
            StartRefusal::AlreadyRunning | StartRefusal::StartLockHeld => EXIT_ALREADY_RUNNING,
            _ => 1,
        }
    }
}

/// The `--json` outcome of a refused start: `(outcome, reason)` in the vocabulary of a caller that
/// must decide whether to retry (Synapse's `/v1/launch-result`, spec C4): `already-running` is
/// the no-op, `refused` is terminal (asking again changes nothing), `failed` may succeed later.
/// Reasons are short codes, never free text.
impl StartRefusal {
    pub fn outcome(&self) -> (&'static str, &'static str) {
        match self {
            StartRefusal::AlreadyRunning => ("already-running", "already-running"),
            StartRefusal::NotPm(_) => ("refused", "not-pm"),
            StartRefusal::PmRole => ("refused", "pm-role"),
            StartRefusal::NoSignal => ("refused", "no-signal"),
            StartRefusal::InvalidRole(_) => ("refused", "invalid-role"),
            StartRefusal::Reason(_) => ("refused", "reason"),
            StartRefusal::Model(_) => ("refused", "model-not-pinned"),
            StartRefusal::NoSpec(_) => ("refused", "no-spec"),
            StartRefusal::SpecUnusable(_) => ("refused", "spec-unusable"),
            StartRefusal::StopNotSoft(reason) => ("refused", reason),
            StartRefusal::StartLockHeld => ("already-running", "start-lock-held"),
            StartRefusal::StartLockUnavailable(_) => ("failed", "start-lock-unavailable"),
            StartRefusal::RateCap(hit) => ("refused", hit.reason()),
            StartRefusal::HeldForMemory { .. } => ("failed", "held-for-memory"),
            StartRefusal::RestoreLockHeld { .. } => ("failed", "lock-held"),
            StartRefusal::GateUnavailable { reason, .. } => ("failed", reason),
            StartRefusal::DuplicateRisk(DuplicateRefusal::Live(_)) => ("failed", "duplicate-risk"),
            StartRefusal::DuplicateRisk(DuplicateRefusal::Unreadable(_)) => {
                ("failed", "process-table-unreadable")
            }
            StartRefusal::Launch(_) => ("failed", "spawn-failed"),
        }
    }
}

impl std::fmt::Display for StartRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartRefusal::NotPm(Ok(role)) => write!(
                f,
                "only the PM session may start a lane; this command is running under {role:?}"
            ),
            StartRefusal::NotPm(Err(e)) => write!(
                f,
                "only the PM session may start a lane, and the caller could not be verified: {e}"
            ),
            StartRefusal::PmRole => write!(
                f,
                "the PM is started by a person, not by lane-restart (it runs in \
                 bypassPermissions, which a start would silently downgrade)"
            ),
            StartRefusal::NoSignal => write!(
                f,
                "the launcher may start a lane only for a mail-waiting signal: --signal-id \
                 <role>:<id> naming the role is required"
            ),
            StartRefusal::InvalidRole(r) => write!(f, "not a plain role identifier: {r:?}"),
            StartRefusal::Reason(why) => write!(f, "{why}"),
            StartRefusal::Model(m) => write!(
                f,
                "--model accepts only sonnet or haiku, not {m:?} (CireSnave: \"I can't afford Opus.\")"
            ),
            StartRefusal::NoSpec(why) => write!(
                f,
                "no launch spec to start from ({why}); run `lane-restart specs import` to build one \
                 from the lane's last state file"
            ),
            StartRefusal::SpecUnusable(why) => write!(f, "the launch spec cannot be started: {why}"),
            StartRefusal::AlreadyRunning => write!(
                f,
                "the lane is already running (a live claude matches its recorded pid and start time)"
            ),
            StartRefusal::DuplicateRisk(d) => write!(f, "{d}"),
            StartRefusal::StartLockHeld => write!(
                f,
                "another start of this role is in progress (its start lock is held)"
            ),
            StartRefusal::StartLockUnavailable(why) => {
                write!(f, "the start lock could not be taken: {why}")
            }
            StartRefusal::StopNotSoft("stop-hard") => write!(
                f,
                "the lane was stopped on purpose (stop_kind hard); only a person or the PM may start it"
            ),
            StartRefusal::StopNotSoft(_) => write!(
                f,
                "the lane's stop is not classified (no stop_kind); the launcher starts only a soft                  stop or a crash"
            ),
            StartRefusal::RateCap(hit) => write!(
                f,
                "the launcher's rate cap is reached: {hit}; a person may start the lane"
            ),
            StartRefusal::HeldForMemory { free_gb, floor_gb } => write!(
                f,
                "held for memory: {free_gb:.1} GB free, floor {floor_gb} GB"
            ),
            StartRefusal::RestoreLockHeld { pid } => write!(
                f,
                "another restore or launcher start holds restore.lock (pid {pid})"
            ),
            StartRefusal::GateUnavailable { why, .. } => {
                write!(f, "a launcher gate could not be read, nothing was started: {why}")
            }
            StartRefusal::Launch(e) => write!(f, "{e}"),
        }
    }
}

/// A start that passed every check: nothing has been spawned yet.
#[derive(Debug, Clone)]
pub struct StartPlan {
    /// What a launch reads: model in the argv, permission mode `auto`.
    pub launch: LaunchSpec,
    /// The model the argv carries.
    pub model: String,
    /// The model this start replaced, when the spec's was Opus, unknown or unset.
    pub pinned_from: Option<String>,
    pub argv: Vec<String>,
    pub carries_dev_channels_flag: bool,
    /// The session recorded by the lane's last state file: dead, and not to be mistaken for the
    /// new one when the launch is watched for progress.
    pub previous_session: Option<String>,
    /// The spec as it will be written back (`running`) once the session is confirmed.
    pub spec: SpecFile,
}

/// Every check, in the order of 14.5; nothing is spawned or written.
pub fn plan_start(
    dir: &Path,
    req: &StartRequest,
    caller: Result<String, CallerError>,
    facts: &dyn SystemFacts,
) -> Result<StartPlan, StartRefusal> {
    let launcher = match caller {
        Ok(role) if role == PM_ROLE => false,
        Ok(caller) if is_launcher_caller(&caller) => true,
        other => return Err(StartRefusal::NotPm(other)),
    };
    if !valid_identifier(req.role) {
        return Err(StartRefusal::InvalidRole(req.role.to_string()));
    }
    if req.role == PM_ROLE {
        return Err(StartRefusal::PmRole);
    }
    if launcher
        && !req
            .signal_id
            .is_some_and(|s| signal_names_role(s, req.role))
    {
        return Err(StartRefusal::NoSignal);
    }
    validate_reason(req.reason).map_err(StartRefusal::Reason)?;
    let requested = match req.model {
        Some(m) => Some(validate_model_request(m).map_err(StartRefusal::Model)?),
        None => None,
    };

    let status = role_status(dir, req.role, facts);
    if status.state == Lifecycle::Running {
        return Err(StartRefusal::AlreadyRunning);
    }
    let from_disk = match spec::read(dir, req.role) {
        Ok(s) => s,
        Err(SpecError::NotFound(p)) => return Err(StartRefusal::NoSpec(p)),
        Err(e) => return Err(StartRefusal::SpecUnusable(e.to_string())),
    };
    if launcher {
        launcher_may_start(&from_disk)?;
    }
    // A session that never reached its `SessionStart` (stuck at a startup dialog) is in no state
    // file: only the process table sees it, whatever the state file says (2026-10-14).
    let spec_name = from_disk.launch_spec().name;
    duplicate::check(facts, req.role, &[req.role, spec_name.as_str()], None)
        .map_err(StartRefusal::DuplicateRisk)?;
    // An explicit request wins; else the spec's own model, replaced when it is not an explicit
    // Sonnet or Haiku. `LANE_RESTART_ALLOW_OPUS` is never read here.
    let mut effective = from_disk.clone();
    let pinned_from = match requested {
        Some(m) => {
            effective.model = m.to_string();
            None
        }
        None => {
            let (p, was) = from_disk.pinned();
            effective.model = p.model;
            was
        }
    };
    effective.permission_mode = Some(START_PERMISSION_MODE.to_string());
    let launch = effective.launch_spec();
    let prepared = prepare_launch(&launch, &effective.model, None).map_err(StartRefusal::Launch)?;
    let previous_session = state::load(dir, req.role).ok().map(|s| s.session_id);
    Ok(StartPlan {
        model: effective.model.clone(),
        pinned_from,
        argv: prepared.argv.to_vec(),
        carries_dev_channels_flag: prepared.carries_dev_channels_flag,
        previous_session,
        launch,
        spec: effective,
    })
}

/// Board 216 (CireSnave: "216: yes"): the launcher starts a lane only if it ended in the course of
/// its own work. A crash needs no mark (`Crashed`, or `Running` with the process gone: the caller
/// has already refused a live one). A stop (`Suspended`, or `Stopping` when the suspend write did
/// not land) must be recorded `soft`; `hard` and unclassified are refused. The spec is the
/// authority, not the agent registry: `agentlife stop --soft` leaves its intent `Closed{exited}`
/// and writes `soft` here, `park` writes `hard`, so the two cannot be read apart from this field.
fn launcher_may_start(spec: &SpecFile) -> Result<(), StartRefusal> {
    match spec.state {
        SpecState::Running | SpecState::Crashed => Ok(()),
        SpecState::Suspended | SpecState::Stopping => match spec.stop_kind {
            Some(spec::StopKind::Soft) => Ok(()),
            Some(spec::StopKind::Hard) => Err(StartRefusal::StopNotSoft("stop-hard")),
            None => Err(StartRefusal::StopNotSoft("stop-unclassified")),
        },
    }
}

/// What `execute_start` achieved.
#[derive(Debug)]
pub struct StartResult {
    pub outcome: RelaunchOutcome,
    /// Set when the lane is up but its spec could not be marked `running`: the previous spec is
    /// still complete, so nothing is lost, but the ledger should say so.
    pub spec_warning: Option<String>,
    /// Wall-clock time spent inside `launch_and_wait_with` only: argument preparation (no I/O),
    /// the spawn and the wait for progress or the dialog hand-off. Excludes planning, the disk gate
    /// and the spec write-back. For an awaiting-dialog hand-off it is not a launch time, and the
    /// caller reports none.
    pub launch_elapsed: Duration,
}

/// Spawns the launch and waits for real progress, then writes the spec back as `running`
/// (`written_by: "launch"`). NO kill: a start never stops anything.
#[allow(clippy::too_many_arguments)]
pub fn execute_start(
    plan: &StartPlan,
    facts: &dyn SystemFacts,
    state_reader: &dyn StateReader,
    specs: &dyn SpecStore,
    timing: &LivenessTiming,
    spawn: &SpawnFn,
    sleep: &mut dyn FnMut(Duration),
    on_awaiting: &mut dyn FnMut(),
) -> Result<StartResult, RelaunchError> {
    let started = std::time::Instant::now();
    let outcome = launch_and_wait_with(
        facts,
        state_reader,
        &plan.launch,
        plan.previous_session.as_deref(),
        timing,
        &plan.model,
        None,
        spawn,
        sleep,
        on_awaiting,
    )?;
    let launch_elapsed = started.elapsed();
    let mut running = plan.spec.clone();
    running.state = SpecState::Running;
    running.written_by = "launch".to_string();
    running.written_at = facts.now();
    // A launch is not a stop: the kind described the stop that ended the previous session.
    running.stop_kind = None;
    running.stopped_session_id = None;
    running.stopped_pid_start_secs = None;
    running.tool_version = env!("CARGO_PKG_VERSION").to_string();
    let spec_warning = specs.write(&running).err().map(|e| e.to_string());
    Ok(StartResult {
        outcome,
        spec_warning,
        launch_elapsed,
    })
}

/// Takes the per-role start lock for `role` (see `start_lock`). `plan_start` must run while the
/// returned guard is held, so its `role_status` and `duplicate::check` are the re-check after the
/// lock. A role that is not an identifier takes no lock: `plan_start` refuses it as `InvalidRole`,
/// and a lock path is never built from it.
pub fn take_start_lock(
    dir: &Path,
    role: &str,
) -> Result<Option<crate::start_lock::StartLock>, StartRefusal> {
    use crate::start_lock::{acquire, StartLockError};
    match acquire(dir, role) {
        Ok(lock) => Ok(Some(lock)),
        Err(StartLockError::InvalidRole) => Ok(None),
        Err(StartLockError::Held) => Err(StartRefusal::StartLockHeld),
        Err(StartLockError::Unavailable(why)) => Err(StartRefusal::StartLockUnavailable(why)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{build, write_atomic, DirSpecStore, SpecInputs};
    use crate::testing::{log, state_at, RecordingFacts, ScriptedReader};
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    const PM_PID: u32 = 900;
    const PM_START: u64 = 5000;

    fn launch(role: &str) -> LaunchSpec {
        LaunchSpec {
            role: role.to_string(),
            name: role.to_string(),
            cwd: "C:/Projects/x".to_string(),
            permission_mode: Some("bypassPermissions".to_string()),
            remote_control: false,
            launch_args: Some(
                [
                    "claude.exe",
                    "--model",
                    "sonnet",
                    "--dangerously-load-development-channels",
                    "server:claude-peers",
                ]
                .map(String::from)
                .to_vec(),
            ),
        }
    }

    fn write_spec(dir: &Path, role: &str, model: &str, st: SpecState) {
        let l = launch(role);
        let mut s = build(&SpecInputs {
            launch: &l,
            model: "sonnet",
            session_id: Some("old-session"),
            pid_start_secs: Some(1),
            state: st,
            written_by: "import",
            now: Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap(),
        })
        .unwrap();
        // the model may be anything the file says; `write_atomic` is strict, so write raw
        s.model = model.to_string();
        let path = dir.join(format!("{role}.launch.json"));
        std::fs::write(path, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
    }

    fn write_state(dir: &Path, role: &str, pid: u32, start: Option<u64>, session: &str) {
        let mut st = state_at(role, session, "PreToolUse");
        st.pid = pid;
        st.pid_start_secs = start;
        st.busy = true;
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    /// The PM is alive as `PM_PID`; the lane's own pid is dead.
    fn world() -> (tempfile::TempDir, RecordingFacts) {
        let d = tempdir().unwrap();
        write_state(d.path(), "pm", PM_PID, Some(PM_START), "pm-session");
        let l = log();
        let mut facts = RecordingFacts::new(&l);
        facts.live_pid = Some((PM_PID, PM_START));
        (d, facts)
    }

    fn pm_caller(dir: &Path, facts: &RecordingFacts) -> Result<String, CallerError> {
        caller_role(dir, Some(PM_PID), facts)
    }

    fn req(role: &str) -> StartRequest<'_> {
        StartRequest {
            role,
            reason: "resume after the 17:34Z crash",
            model: None,
            signal_id: None,
        }
    }

    const LAUNCHER_PID: u32 = 7000;
    const LAUNCHER_START: u64 = 8000;

    /// Writes `<file_role>.launcher-caller` holding a record that names `role`.
    fn write_launcher_record_in(dir: &Path, file_role: &str, role: &str, pid: u32, start: u64) {
        let rec = LauncherRecord {
            role: role.into(),
            pid,
            pid_start_secs: start,
        };
        std::fs::write(
            dir.join(launcher_record_name(file_role)),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
    }

    fn write_launcher_record(dir: &Path, role: &str, pid: u32, start: u64) {
        write_launcher_record_in(dir, role, role, pid, start);
    }

    #[test]
    fn a_held_start_lock_is_the_already_running_noop_and_an_unavailable_one_is_failed() {
        let d = tempdir().unwrap();
        let _held = take_start_lock(d.path(), "lightbulb").unwrap().unwrap();
        let second = take_start_lock(d.path(), "lightbulb").unwrap_err();
        assert_eq!(second.outcome(), ("already-running", "start-lock-held"));
        assert_eq!(second.exit_code(), EXIT_ALREADY_RUNNING);
        // another role is unaffected; an invalid role takes no lock (plan_start refuses it)
        assert!(take_start_lock(d.path(), "coderipper").unwrap().is_some());
        assert!(take_start_lock(d.path(), "../pm").unwrap().is_none());
        let gone = d.path().join("nope");
        let un = take_start_lock(&gone, "lightbulb").unwrap_err();
        assert_eq!(un.outcome(), ("failed", "start-lock-unavailable"));
        assert_eq!(un.exit_code(), 1);
    }

    #[test]
    fn the_launcher_is_recognised_by_its_record_pid_and_start_time() {
        let (d, mut facts) = world();
        write_launcher_record(d.path(), LAUNCHER_ROLE, LAUNCHER_PID, LAUNCHER_START);
        facts.live_pid = Some((LAUNCHER_PID, LAUNCHER_START));
        assert_eq!(
            launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).as_deref(),
            Ok("launcher:agentlife-launcher")
        );
        // the 0.15.2 name still works for one release
        std::fs::remove_file(d.path().join(launcher_record_name(LAUNCHER_ROLE))).unwrap();
        write_launcher_record(d.path(), LEGACY_LAUNCHER_ROLE, LAUNCHER_PID, LAUNCHER_START);
        assert_eq!(
            launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).as_deref(),
            Ok("launcher:overmind")
        );
        // a record filed under one name but naming another is not either of them
        std::fs::remove_file(d.path().join(launcher_record_name(LEGACY_LAUNCHER_ROLE))).unwrap();
        write_launcher_record_in(
            d.path(),
            LAUNCHER_ROLE,
            LEGACY_LAUNCHER_ROLE,
            LAUNCHER_PID,
            LAUNCHER_START,
        );
        assert!(launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).is_err());
        std::fs::remove_file(d.path().join(launcher_record_name(LAUNCHER_ROLE))).unwrap();
        write_launcher_record(d.path(), LAUNCHER_ROLE, LAUNCHER_PID, LAUNCHER_START);
        // a recycled pid: same number, other start time
        facts.live_pid = Some((LAUNCHER_PID, LAUNCHER_START + 1));
        assert!(launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).is_err());
        // some other process claiming to be the service
        facts.live_pid = Some((LAUNCHER_PID, LAUNCHER_START));
        assert!(launcher_caller(d.path(), Some(LAUNCHER_PID + 1), &facts).is_err());
        // a record for another role is not the launcher
        write_launcher_record_in(
            d.path(),
            LAUNCHER_ROLE,
            "baracuda",
            LAUNCHER_PID,
            LAUNCHER_START,
        );
        assert!(launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).is_err());
        // no record at all
        std::fs::remove_file(d.path().join(launcher_record_name(LAUNCHER_ROLE))).unwrap();
        assert!(launcher_caller(d.path(), Some(LAUNCHER_PID), &facts).is_err());
    }

    #[test]
    fn the_launcher_may_start_only_for_a_signal_that_names_the_role() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let launcher = || Ok("launcher:agentlife-launcher".to_string());
        let mut r = req("baracuda");
        // positive control: the PM needs no signal and gets past the same gate
        assert!(!matches!(
            plan_start(d.path(), &r, Ok("pm".to_string()), &facts),
            Err(StartRefusal::NoSignal)
        ));
        for bad in [
            None,
            Some(""),
            Some("baracuda"),
            Some("baracuda:"),
            Some("fuel:12"),
            Some("baracudax:1"),
            Some("baracuda:1;2"),
        ] {
            r.signal_id = bad;
            let e = plan_start(d.path(), &r, launcher(), &facts).unwrap_err();
            assert!(matches!(e, StartRefusal::NoSignal), "{bad:?}: {e:?}");
        }
        r.signal_id = Some("baracuda:42");
        let e = plan_start(d.path(), &r, launcher(), &facts);
        assert!(
            !matches!(e, Err(StartRefusal::NoSignal) | Err(StartRefusal::NotPm(_))),
            "{e:?}"
        );
    }

    fn set_kind(dir: &Path, role: &str, kind: Option<&str>) {
        let p = dir.join(format!("{role}.launch.json"));
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        match kind {
            Some(k) => v["stop_kind"] = k.into(),
            None => {
                v.as_object_mut().unwrap().remove("stop_kind");
            }
        }
        std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    #[test]
    fn the_launcher_starts_only_a_soft_stop_or_a_crash_and_the_pm_is_unaffected() {
        let launcher = || Ok("launcher:agentlife-launcher".to_string());
        let mut r = req("baracuda");
        r.signal_id = Some("baracuda:42");
        let cases = [
            (SpecState::Suspended, Some("soft"), None),
            (SpecState::Suspended, Some("hard"), Some("stop-hard")),
            (SpecState::Suspended, None, Some("stop-unclassified")),
            (SpecState::Stopping, Some("soft"), None),
            (SpecState::Stopping, Some("hard"), Some("stop-hard")),
            (SpecState::Stopping, None, Some("stop-unclassified")),
            // a crash is soft by nature: no mark needed, and a stale kind is not read
            (SpecState::Crashed, None, None),
            (SpecState::Crashed, Some("hard"), None),
            (SpecState::Running, None, None),
        ];
        for (state, kind, want) in cases {
            let (d, facts) = world();
            write_spec(d.path(), "baracuda", "sonnet", state);
            set_kind(d.path(), "baracuda", kind);
            let got = plan_start(d.path(), &r, launcher(), &facts);
            match (want, &got) {
                (None, Ok(_)) => {}
                (Some(reason), Err(e)) => {
                    assert_eq!(e.outcome(), ("refused", reason), "{state:?} {kind:?}");
                    assert_eq!(e.exit_code(), 1);
                }
                _ => panic!("{state:?} {kind:?}: wanted {want:?}, got {got:?}"),
            }
            // the PM may start the same lane whatever the kind
            assert!(
                plan_start(
                    d.path(),
                    &req("baracuda"),
                    pm_caller(d.path(), &facts),
                    &facts
                )
                .is_ok(),
                "{state:?} {kind:?}"
            );
        }
    }

    #[test]
    fn the_launcher_cannot_start_the_pm_and_other_roles_are_still_not_pm() {
        let (d, facts) = world();
        let mut r = req("pm");
        r.signal_id = Some("pm:1");
        assert!(matches!(
            plan_start(
                d.path(),
                &r,
                Ok("launcher:agentlife-launcher".to_string()),
                &facts
            ),
            Err(StartRefusal::PmRole)
        ));
        // the legacy name is the same gate
        assert!(matches!(
            plan_start(d.path(), &r, Ok("launcher:overmind".to_string()), &facts),
            Err(StartRefusal::PmRole)
        ));
    }

    /// A *lane* whose role is `overmind` or `agentlife-launcher` (found by process ancestry, as
    /// the OverMind lane would be) is an ordinary non-PM caller: only a launcher record counts.
    #[test]
    fn a_lane_that_shares_the_launchers_name_is_not_the_launcher() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let mut r = req("baracuda");
        r.signal_id = Some("baracuda:42");
        for lane in ["overmind", "agentlife-launcher"] {
            let e = plan_start(d.path(), &r, Ok(lane.to_string()), &facts).unwrap_err();
            assert!(matches!(e, StartRefusal::NotPm(_)), "{lane}: {e:?}");
        }
        // positive control: the same request from a validated launcher record passes the gate
        let e = plan_start(
            d.path(),
            &r,
            Ok("launcher:agentlife-launcher".to_string()),
            &facts,
        );
        assert!(!matches!(e, Err(StartRefusal::NotPm(_))), "{e:?}");
        assert!(matches!(
            plan_start(d.path(), &req("baracuda"), Ok("fuel".to_string()), &facts),
            Err(StartRefusal::NotPm(_))
        ));
    }

    // ---- caller ----------------------------------------------------------------------------

    #[test]
    fn the_caller_is_the_role_whose_state_file_records_the_claude_ancestor() {
        let (d, facts) = world();
        assert_eq!(pm_caller(d.path(), &facts), Ok("pm".to_string()));
    }

    /// Positive control above; here the pid is a claude but belongs to no state file.
    #[test]
    fn an_unrecorded_claude_is_not_a_known_caller() {
        let (d, facts) = world();
        assert_eq!(
            caller_role(d.path(), Some(1234), &facts),
            Err(CallerError::Unrecognised(1234))
        );
    }

    #[test]
    fn no_claude_ancestor_is_no_caller() {
        let (d, facts) = world();
        assert_eq!(
            caller_role(d.path(), None, &facts),
            Err(CallerError::NoClaudeAncestor)
        );
    }

    /// The pid is the PM's but the process at that pid started at another time: a recycled pid.
    #[test]
    fn a_recycled_pid_is_not_the_pm() {
        let (d, mut facts) = world();
        facts.live_pid = Some((PM_PID, PM_START + 77));
        assert_eq!(
            caller_role(d.path(), Some(PM_PID), &facts),
            Err(CallerError::Unrecognised(PM_PID))
        );
    }

    #[test]
    fn two_state_files_claiming_one_pid_are_refused_not_chosen_between() {
        let (d, facts) = world();
        write_state(d.path(), "agentlife", PM_PID, Some(PM_START), "s");
        assert_eq!(
            caller_role(d.path(), Some(PM_PID), &facts),
            Err(CallerError::Ambiguous(PM_PID))
        );
    }

    #[test]
    fn a_launch_spec_in_the_directory_is_never_a_caller() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        assert_eq!(pm_caller(d.path(), &facts), Ok("pm".to_string()));
    }

    // ---- reason / model --------------------------------------------------------------------

    #[test]
    fn a_reason_must_be_present_short_and_free_of_control_characters() {
        assert_eq!(
            validate_reason("  resume baracuda  ").unwrap(),
            "resume baracuda"
        );
        assert!(validate_reason("").is_err());
        assert!(validate_reason("   \t ").is_err());
        assert!(validate_reason("a\nb").is_err());
        assert!(validate_reason(&"x".repeat(200)).is_ok());
        assert!(validate_reason(&"x".repeat(201)).is_err());
    }

    #[test]
    fn only_sonnet_and_haiku_may_be_requested() {
        assert_eq!(validate_model_request("sonnet"), Ok("sonnet"));
        assert_eq!(validate_model_request("haiku"), Ok("haiku"));
        assert!(validate_model_request("opus").is_err());
        assert!(validate_model_request("claude-opus-5-5").is_err());
        assert!(validate_model_request("").is_err());
    }

    // ---- plan_start ------------------------------------------------------------------------

    #[test]
    fn a_suspended_lane_is_planned_with_auto_permission_and_the_specs_flags() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let p = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        let a = &p.argv;
        let i = a.iter().position(|x| x == "--permission-mode").unwrap();
        assert_eq!(a[i + 1], "auto", "{a:?}");
        let m = a.iter().position(|x| x == "--model").unwrap();
        assert_eq!(a[m + 1], "sonnet");
        assert!(a.contains(&"--dangerously-load-development-channels".to_string()));
        assert!(p.carries_dev_channels_flag);
        assert_eq!(p.pinned_from, None);
        assert_eq!(a[1], "read baracuda HANDOFF and continue");
    }

    /// The bypassPermissions the spec recorded is replaced, never carried to a start.
    #[test]
    fn the_recorded_bypass_permission_mode_is_not_launched() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let p = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        assert!(!p.argv.iter().any(|a| a.contains("bypass")), "{:?}", p.argv);
    }

    #[test]
    fn a_non_pm_caller_is_refused_and_a_pm_caller_passes_the_same_inputs() {
        let (d, facts) = world();
        write_state(d.path(), "agentlife", 77, Some(1), "s");
        let mut f = RecordingFacts::new(&log());
        f.live_pid = Some((77, 1));
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let as_lane = caller_role(d.path(), Some(77), &f);
        assert_eq!(as_lane, Ok("agentlife".to_string()));
        let refused = plan_start(d.path(), &req("baracuda"), as_lane, &f);
        assert!(matches!(refused, Err(StartRefusal::NotPm(Ok(ref r))) if r == "agentlife"));
        // control: the PM, same directory, same request
        assert!(plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts
        )
        .is_ok());
    }

    #[test]
    fn an_unverifiable_caller_is_refused() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            Err(CallerError::NoClaudeAncestor),
            &facts,
        );
        assert!(matches!(r, Err(StartRefusal::NotPm(Err(_)))));
    }

    #[test]
    fn starting_the_pm_is_refused_and_reported() {
        let (d, facts) = world();
        write_spec(d.path(), "pm", "sonnet", SpecState::Suspended);
        let r = plan_start(d.path(), &req("pm"), pm_caller(d.path(), &facts), &facts);
        assert!(matches!(r, Err(StartRefusal::PmRole)), "{r:?}");
    }

    #[test]
    fn a_missing_or_blank_reason_refuses_before_the_spec_is_read() {
        let (d, facts) = world();
        // no spec on disk at all: the reason refusal must come first, not NoSpec
        let mut r = req("baracuda");
        r.reason = "  ";
        let out = plan_start(d.path(), &r, pm_caller(d.path(), &facts), &facts);
        assert!(matches!(out, Err(StartRefusal::Reason(_))), "{out:?}");
    }

    #[test]
    fn an_opus_request_is_refused() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let mut r = req("baracuda");
        r.model = Some("opus");
        let out = plan_start(d.path(), &r, pm_caller(d.path(), &facts), &facts);
        assert!(matches!(out, Err(StartRefusal::Model(_))), "{out:?}");
    }

    /// A spec whose model is Opus is launched on sonnet, and says what it replaced.
    #[test]
    fn an_opus_spec_is_pinned_to_sonnet_and_the_replacement_is_reported() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "claude-opus-5-5", SpecState::Crashed);
        let p = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        assert_eq!(p.model, "sonnet");
        assert_eq!(p.pinned_from.as_deref(), Some("claude-opus-5-5"));
        assert!(!p
            .argv
            .iter()
            .any(|a| a.to_ascii_lowercase().contains("opus")));
    }

    #[test]
    fn an_explicit_haiku_request_is_used() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let mut r = req("baracuda");
        r.model = Some("haiku");
        let p = plan_start(d.path(), &r, pm_caller(d.path(), &facts), &facts).unwrap();
        assert_eq!(p.model, "haiku");
        assert_eq!(p.pinned_from, None);
    }

    #[test]
    fn a_lane_with_no_spec_is_refused_naming_specs_import() {
        let (d, facts) = world();
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        let Err(StartRefusal::NoSpec(_)) = r else {
            panic!("{r:?}")
        };
        assert!(r.unwrap_err().to_string().contains("specs import"));
    }

    #[test]
    fn a_running_lane_is_not_started_twice() {
        let (d, mut facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Running);
        write_state(d.path(), "baracuda", 55, Some(100), "live-session");
        facts.live_pid = Some((55, 100));
        // the PM is not the live pid any more in this facts; the caller is passed verified
        let r = plan_start(d.path(), &req("baracuda"), Ok("pm".to_string()), &facts);
        assert!(matches!(r, Err(StartRefusal::AlreadyRunning)), "{r:?}");
    }

    /// OverMind's contract: "already running" is exit 3, distinct from every other refusal (1).
    #[test]
    fn json_outcomes_separate_terminal_refusals_from_retryable_failures() {
        assert_eq!(
            StartRefusal::AlreadyRunning.outcome(),
            ("already-running", "already-running")
        );
        assert_eq!(StartRefusal::NoSpec("x".into()).outcome().0, "refused");
        assert_eq!(
            StartRefusal::Model("opus".into()).outcome().1,
            "model-not-pinned"
        );
        assert_eq!(
            StartRefusal::NotPm(Err(CallerError::NoClaudeAncestor)).outcome(),
            ("refused", "not-pm")
        );
        assert_eq!(
            StartRefusal::DuplicateRisk(DuplicateRefusal::Unreadable("x".into())).outcome(),
            ("failed", "process-table-unreadable")
        );
    }

    #[test]
    fn already_running_has_its_own_exit_code_and_other_refusals_stay_one() {
        assert_eq!(EXIT_ALREADY_RUNNING, 3);
        assert_eq!(StartRefusal::AlreadyRunning.exit_code(), 3);
        assert_eq!(StartRefusal::PmRole.exit_code(), 1);
        assert_eq!(StartRefusal::NoSpec("x".into()).exit_code(), 1);
        assert_eq!(StartRefusal::Model("opus".into()).exit_code(), 1);
        assert_eq!(
            StartRefusal::NotPm(Err(CallerError::NoClaudeAncestor)).exit_code(),
            1
        );
        assert_eq!(
            StartRefusal::DuplicateRisk(DuplicateRefusal::Unreadable("x".into())).exit_code(),
            1
        );
    }

    /// The 4775 s case: the dead lane's state file is hours old and says nothing about being
    /// startable; its spec is the way back, whatever the file's age.
    #[test]
    fn a_crashed_lane_with_an_ancient_state_file_is_startable() {
        let (d, facts) = world();
        let mut st = state_at("baracuda", "dead-session", "PreToolUse");
        st.pid = 19184;
        st.busy = true;
        st.updated_at = Utc::now() - chrono::Duration::seconds(4775);
        std::fs::write(
            d.path().join("baracuda.json"),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Running);
        let p = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        assert_eq!(p.previous_session.as_deref(), Some("dead-session"));
    }

    #[test]
    fn a_semicolon_in_the_specs_cwd_is_refused_before_anything_is_spawned() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let path = d.path().join("baracuda.launch.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["cwd"] = "C:/a;b".into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        assert!(
            matches!(
                r,
                Err(StartRefusal::Launch(RelaunchError::UnsafeArgument(_)))
            ),
            "{r:?}"
        );
    }

    #[test]
    fn a_newer_schema_spec_is_not_started() {
        let (d, facts) = world();
        std::fs::write(
            d.path().join("baracuda.launch.json"),
            r#"{"schema": 9, "role": "baracuda"}"#,
        )
        .unwrap();
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        assert!(matches!(r, Err(StartRefusal::SpecUnusable(_))), "{r:?}");
    }

    // ---- double-launch guard ---------------------------------------------------------------

    fn proc(pid: u32, parent: u32, name: &str, cmd: &[&str]) -> crate::facts::ProcEntry {
        crate::facts::ProcEntry {
            pid,
            parent: Some(parent),
            name: name.to_string(),
            start_time_secs: 9000,
            exe: None,
            cmd: cmd.iter().map(ToString::to_string).collect(),
        }
    }

    fn unisql_session() -> Vec<crate::facts::ProcEntry> {
        vec![
            proc(
                3000,
                1,
                "lane-restart.exe",
                &[
                    "lane-restart.exe",
                    "host",
                    "--role",
                    "baracuda",
                    "--",
                    "claude.exe",
                ],
            ),
            proc(
                3001,
                3000,
                "claude.exe",
                &["claude.exe", "--name", "baracuda", "--model", "sonnet"],
            ),
        ]
    }

    /// Case 1: the first session is alive but stuck at the startup dialog, so no state file
    /// names it (there is none for the role at all). Only the process table knows.
    #[test]
    fn a_live_host_stuck_at_the_startup_dialog_refuses_a_start_with_no_state_file() {
        let (d, mut facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        // control: the same plan with an empty process table is accepted
        assert!(plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts
        )
        .is_ok());
        facts.table = Ok(unisql_session());
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        let Err(StartRefusal::DuplicateRisk(DuplicateRefusal::Live(ref v))) = r else {
            panic!("{r:?}")
        };
        assert_eq!(
            v.iter().map(|x| x.pid).collect::<Vec<_>>(),
            vec![3000, 3001]
        );
        let msg = r.unwrap_err().to_string();
        assert!(
            msg.contains("host pid 3000") && msg.contains("claude pid 3001"),
            "{msg}"
        );
    }

    /// Case 2: the state file exists but is stale (crashed; its pid is dead), and a host for the
    /// role is alive anyway. The state file saying "crashed" must not make it startable.
    #[test]
    fn a_stale_crashed_state_file_does_not_hide_a_live_host() {
        let (d, mut facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Crashed);
        write_state(d.path(), "baracuda", 19184, Some(1), "dead-session");
        // the stale state reads as not running, so only the guard can refuse
        assert_eq!(
            role_status(d.path(), "baracuda", &facts).state,
            Lifecycle::Crashed
        );
        facts.table = Ok(vec![unisql_session().remove(0)]);
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        assert!(matches!(r, Err(StartRefusal::DuplicateRisk(_))), "{r:?}");
    }

    #[test]
    fn an_unreadable_process_table_refuses_a_start() {
        let (d, mut facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        facts.table = Err("access denied".into());
        let r = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        );
        assert!(
            matches!(
                r,
                Err(StartRefusal::DuplicateRisk(DuplicateRefusal::Unreadable(_)))
            ),
            "{r:?}"
        );
    }

    // ---- execute_start ---------------------------------------------------------------------

    /// The dead session's own later events must not read as the new session coming up.
    #[test]
    fn the_dead_sessions_state_does_not_count_as_the_new_session() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Crashed);
        write_state(d.path(), "baracuda", 19184, Some(1), "dead-session");
        let plan = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        let l = log();
        let mut f = RecordingFacts::new(&l);
        f.alive = false;
        // the reader keeps returning the DEAD session at an event past SessionStart
        let reader = ScriptedReader::new(
            &l,
            vec![Some(state_at("baracuda", "dead-session", "PreToolUse"))],
        );
        let short = LivenessTiming {
            progress_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_secs(1),
        };
        let out = execute_start(
            &plan,
            &f,
            &reader,
            &DirSpecStore {
                dir: d.path().to_path_buf(),
            },
            &short,
            &|_, _| Ok(()),
            &mut |_| {},
            &mut || {},
        );
        assert!(
            matches!(out, Err(RelaunchError::SessionNeverProcessedPrompt)),
            "{out:?}"
        );
        // and the spec was NOT marked running
        assert_eq!(
            spec::read(d.path(), "baracuda").unwrap().state,
            SpecState::Crashed
        );
    }

    #[test]
    fn a_confirmed_start_writes_the_spec_back_as_running_by_launch() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "claude-opus-5-5", SpecState::Crashed);
        set_kind(d.path(), "baracuda", Some("hard"));
        let plan = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        let l = log();
        let f = RecordingFacts::new(&l);
        let reader = ScriptedReader::new(
            &l,
            vec![Some(state_at(
                "baracuda",
                "new-session",
                "UserPromptSubmit",
            ))],
        );
        let r = execute_start(
            &plan,
            &f,
            &reader,
            &DirSpecStore {
                dir: d.path().to_path_buf(),
            },
            &LivenessTiming::default(),
            &|_, _| Ok(()),
            &mut |_| {},
            &mut || {},
        )
        .unwrap();
        assert_eq!(r.outcome, RelaunchOutcome::Relaunched);
        assert_eq!(r.spec_warning, None);
        let s = spec::read(d.path(), "baracuda").unwrap();
        assert_eq!(s.state, SpecState::Running);
        assert_eq!(s.written_by, "launch");
        assert_eq!(s.model, "sonnet", "the pinned model is what is recorded");
        assert_eq!(s.permission_mode.as_deref(), Some("auto"));
        assert_eq!(
            s.stop_kind, None,
            "a launch is unclassified, not the old stop's kind"
        );
        // a start never kills
        assert!(
            !l.borrow().iter().any(|e| e.starts_with("kill")),
            "{:?}",
            l.borrow()
        );
    }

    #[test]
    fn a_spawn_failure_leaves_the_spec_as_it_was() {
        let (d, facts) = world();
        write_spec(d.path(), "baracuda", "sonnet", SpecState::Suspended);
        let plan = plan_start(
            d.path(),
            &req("baracuda"),
            pm_caller(d.path(), &facts),
            &facts,
        )
        .unwrap();
        let l = log();
        let f = RecordingFacts::new(&l);
        let reader = ScriptedReader::new(&l, vec![None]);
        let out = execute_start(
            &plan,
            &f,
            &reader,
            &DirSpecStore {
                dir: d.path().to_path_buf(),
            },
            &LivenessTiming::default(),
            &|_, _| Err(RelaunchError::Spawn("no wt".into())),
            &mut |_| {},
            &mut || {},
        );
        assert!(matches!(out, Err(RelaunchError::Spawn(_))));
        assert_eq!(
            spec::read(d.path(), "baracuda").unwrap().state,
            SpecState::Suspended
        );
    }

    #[test]
    fn a_complete_spec_round_trips_through_plan_and_the_atomic_write() {
        // control for the raw writes above: a strict write of the same shape is accepted
        let d = tempdir().unwrap();
        let l = launch("baracuda");
        let s = build(&SpecInputs {
            launch: &l,
            model: "sonnet",
            session_id: None,
            pid_start_secs: None,
            state: SpecState::Suspended,
            written_by: "import",
            now: Utc::now(),
        })
        .unwrap();
        write_atomic(d.path(), &s).unwrap();
        assert_eq!(spec::read(d.path(), "baracuda").unwrap(), s);
    }
}
