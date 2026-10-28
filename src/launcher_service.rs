// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife launcher`: the always-on launch-on-message service, **notifier-only** build
//! (`docs/LAUNCHER-SERVICE.md`, build order step 1).
//!
//! The decision logic is the `launcher` crate's (`Engine`, `service::cycle`); this module is the
//! seam to the real world: the Synapse daemon through `synapsed-client`, the registry and launch
//! specs for "is this a lane that may be launched", the process table, the PM notification, one
//! OS advisory lock and the caller record that lets `lane-restart start` recognise the service.
//!
//! It never starts a lane: the mechanism it hands the engine always fails closed, and rule 7
//! (no standing grant, M5 unbuilt, board 216) answers `refused: no-grant` before it could be
//! called.

use crate::identity::{ProcessTable, SysinfoTable};
use crate::list::{liveness, Liveness};
use crate::registry::{Intent, Registry};
use lane_restart::start::{launcher_record_name, LauncherRecord, LAUNCHER_ROLE};
use launcher::service::{Notify, Source, SourceError};
use launcher::{Directory, Lane, LaunchResult, MechResult, Mechanism, Outcome, Signal};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Synapse's `launch_wait` default. Provisional and UNMEASURED (N=0 real claims): it only paces
/// the stale-`started` alarm, which cannot fire in notifier-only mode.
pub const LAUNCH_WAIT_SECS: i64 = 120;
/// The long-poll hold, at the daemon's own cap.
pub const POLL_WAIT_SECS: u64 = 30;
/// The role stall alarms go to.
pub const ALARM_ROLE: &str = "pm";

/// The store file in the lane state directory.
pub fn store_path(state_dir: &Path) -> PathBuf {
    state_dir.join("launcher-store.json")
}

/// The instance lock file in the lane state directory.
pub fn lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join("launcher.lock")
}

// ---------------------------------------------------------------------------------------------
// The daemon.

/// Signals out of a `/v1/waiting` reply (`{"roles": [...]}`). An entry that does not parse (no
/// id, no attempt) cannot be answered, so it is counted and skipped, never guessed at. Only the
/// four fields of [`Signal`] are read: nothing else in an entry is trusted or kept.
pub fn parse_signals(reply: &Value) -> (Vec<Signal>, usize) {
    let mut ok = Vec::new();
    let mut skipped = 0;
    match reply.get("roles").and_then(Value::as_array) {
        Some(entries) => {
            for e in entries {
                match serde_json::from_value::<Signal>(e.clone()) {
                    Ok(s) => ok.push(s),
                    Err(_) => skipped += 1,
                }
            }
        }
        None => skipped += 1,
    }
    (ok, skipped)
}

/// `launch_senders_set` from `GET /v1/stats` (synapsed rc.178 and later). `None` when the field is
/// absent or not a boolean, which includes a daemon too old to have an allowlist: not a yes.
pub fn parse_senders_set(reply: &Value) -> Option<bool> {
    reply.get("launch_senders_set").and_then(Value::as_bool)
}

/// Role names `/v1/list` shows `online` (`{"roles": [{"global_id": "<role>@<account>", ...}]}`).
pub fn parse_online(reply: &Value) -> BTreeSet<String> {
    reply
        .get("roles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("online").and_then(Value::as_bool) == Some(true))
        .filter_map(|e| e.get("global_id").and_then(Value::as_str))
        .filter_map(|g| g.split('@').next())
        .map(str::to_string)
        .collect()
}

/// A daemon refusal as the cycle understands it.
pub fn map_error(e: synapsed_client::MailError) -> SourceError {
    use synapsed_client::MailError as M;
    match e {
        M::Api { kind, message, .. } => match kind.as_str() {
            "unauthorized" | "forbidden" | "superseded" => {
                SourceError::Unauthorized(format!("{kind}: {message}"))
            }
            "stale_signal" => SourceError::Stale,
            _ => SourceError::Unavailable(format!("{kind}: {message}")),
        },
        other => SourceError::Unavailable(other.to_string()),
    }
}

/// The `outcome` string of `POST /v1/launch-result`: the client's own wire string for [`launch_outcome`].
pub fn outcome_wire(o: Outcome) -> &'static str {
    launch_outcome(o).as_str()
}

/// The client's typed outcome for `POST /v1/launch-result`. Synapse's daemon accepts EXACTLY these
/// four (anything else is a 400 `malformed`), and the client's enum can name no other; `dry-run` is
/// never posted.
pub fn launch_outcome(o: Outcome) -> synapsed_client::LaunchOutcome {
    use synapsed_client::LaunchOutcome as W;
    match o {
        Outcome::Started => W::Started,
        Outcome::AlreadyRunning => W::AlreadyRunning,
        Outcome::Refused => W::Refused,
        Outcome::Failed => W::Failed,
    }
}

/// The real daemon, as role [`LAUNCHER_ROLE`].
pub struct SynapseSource {
    daemon: synapsed_client::Daemon,
    /// The home's account: `/v1/send` wants `role@account` (measured on a scratch daemon
    /// 2026-10-17: a bare `pm` is `unknown_recipient`).
    account: String,
    notes: std::cell::RefCell<Vec<String>>,
}

impl SynapseSource {
    /// Finds this home's daemon (starting `synapsed` if nothing answers, as every Synapse client
    /// does) and proves it.
    pub fn connect(home: &Path) -> Result<Self, String> {
        let account = account_of(home)?;
        synapsed_client::Daemon::connect(home)
            .map(|daemon| SynapseSource {
                daemon,
                account,
                notes: std::cell::RefCell::new(Vec::new()),
            })
            .map_err(|e| e.to_string())
    }

    /// Holds a valid session for the role: the cached one, else a claim. The caller record is
    /// written only after this succeeds.
    /// Sends one message to `to_role` as [`LAUNCHER_ROLE`]. `message_id` is the caller's, so a
    /// resend is a duplicate at the daemon, not a second copy.
    pub fn send(&self, to_role: &str, message_id: &str, text: &str) -> Result<(), String> {
        self.daemon
            .call(
                LAUNCHER_ROLE,
                "/v1/send",
                Some(&send_body(
                    &qualify(to_role, &self.account),
                    message_id,
                    text,
                )),
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn session_ok(&self) -> Result<(), SourceError> {
        self.daemon
            .session_info(LAUNCHER_ROLE)
            .map(|_| ())
            .map_err(map_error)
    }
}

impl Source for SynapseSource {
    fn waiting(&self, wait_secs: u64) -> Result<Vec<Signal>, SourceError> {
        let reply = self
            .daemon
            .waiting(LAUNCHER_ROLE, wait_secs)
            .map_err(map_error)?;
        let (signals, skipped) = parse_signals(&reply);
        if skipped > 0 {
            // The readable entries are still answered; the unreadable ones are named in the log
            // every poll they stay (they cannot be answered, so they are never dropped silently).
            self.notes.borrow_mut().push(format!(
                "waiting: {skipped} entr{} could not be read (no id, attempt or epoch) and was not answered",
                if skipped == 1 { "y" } else { "ies" }
            ));
        }
        if skipped > 0 && signals.is_empty() {
            return Err(SourceError::Unavailable(format!(
                "{skipped} waiting entries could not be read"
            )));
        }
        Ok(signals)
    }

    fn post(&self, r: &LaunchResult) -> Result<(), SourceError> {
        let outcome = launch_outcome(r.outcome);
        let reason = (!r.reason.is_empty()).then_some(r.reason.as_str());
        self.daemon
            .launch_result(
                LAUNCHER_ROLE,
                &r.signal_id,
                &r.epoch,
                r.attempt,
                &r.role,
                outcome,
                reason,
            )
            .map(|_| ())
            .map_err(map_error)
    }

    fn clear(&self, role: &str) -> Result<(), SourceError> {
        self.daemon
            .launch_clear(LAUNCHER_ROLE, role)
            .map(|_| ())
            .map_err(map_error)
    }

    fn online(&self) -> Result<BTreeSet<String>, SourceError> {
        self.daemon
            .call(LAUNCHER_ROLE, "/v1/list", None)
            .map(|v| parse_online(&v))
            .map_err(map_error)
    }

    fn senders_set(&self) -> Result<bool, SourceError> {
        let stats = self
            .daemon
            .call(LAUNCHER_ROLE, "/v1/stats", None)
            .map_err(map_error)?;
        parse_senders_set(&stats)
            .ok_or_else(|| SourceError::Unavailable("stats has no launch_senders_set".into()))
    }

    fn drain_notes(&self) -> Vec<String> {
        std::mem::take(&mut *self.notes.borrow_mut())
    }
}

// ---------------------------------------------------------------------------------------------
// What the engine asks about the machine.

/// Rule 3 and the intent rule, read from the registry and the launch specs: launchable only if
/// registry intent is `wanted` or `lazy` AND the spec is neither `suspended` nor `stopping`.
pub struct RegistryDirectory<'a> {
    pub registry: &'a Registry,
    pub lane_state_dir: &'a Path,
    pub table: &'a dyn ProcessTable,
    /// The config's `lazy_enabled` (ships off). Off, a soft stop is a stop like any other here and
    /// rule 7 grants nothing, exactly as before the implied grant (board 216) was read.
    pub wake_enabled: bool,
}

/// What a lane's launch spec says to the launcher, by the predicate `lane-restart start` applies
/// itself (`launcher_may_start`), so the two cannot disagree.
enum SpecVerdict {
    /// No spec file at all: the registry decides (`lane-restart start` refuses a role with no spec).
    Absent,
    /// A spec that exists but may not be used: it records a hard or unclassified stop, or is
    /// corrupt, incomplete, another role's, or of a newer schema. Fails closed.
    Blocked,
    /// A spec the launcher may start from. `soft_stop` is true when it records a soft stop
    /// (`suspended`/`stopping` with `stop_kind` soft), false for a crash or a lane marked running.
    Startable { soft_stop: bool },
}

impl RegistryDirectory<'_> {
    /// The spec stops the launcher: it records a stop, or it exists but cannot be used (corrupt,
    /// incomplete, another role's, a newer schema). Only a spec that is genuinely absent leaves
    /// the decision to the registry (`lane-restart start` itself refuses a role with no spec).
    /// An unusable spec fails closed because it may be hiding a deliberate stop.
    fn spec_verdict(&self, role: &str) -> SpecVerdict {
        use lane_restart::spec::{SpecError, SpecState};
        match lane_restart::spec::read(self.lane_state_dir, role) {
            Err(SpecError::NotFound(_)) => SpecVerdict::Absent,
            Err(_) => SpecVerdict::Blocked,
            Ok(s) => {
                if s.role.as_str() != role
                    || s.check().is_err()
                    || lane_restart::start::launcher_may_start(&s).is_err()
                {
                    return SpecVerdict::Blocked;
                }
                SpecVerdict::Startable {
                    soft_stop: matches!(s.state, SpecState::Suspended | SpecState::Stopping),
                }
            }
        }
    }

    fn records(&self, role: &str) -> Option<Vec<crate::registry::AgentRecord>> {
        let listing = self.registry.list().ok()?;
        Some(
            listing
                .records
                .into_iter()
                .filter(|r| r.spec_role().as_deref() == Some(role))
                .collect(),
        )
    }
}

impl Directory for RegistryDirectory<'_> {
    fn lane(&self, role: &str) -> Lane {
        let Some(recs) = self.records(role) else {
            // An unreadable registry launches nothing.
            return Lane::Unknown;
        };
        // The role's intent is its CURRENT record's: the registry keeps one record per agent, so a
        // parked old agent and a wanted newer one can share a role, and the old row must not
        // override the lane that is wanted now. The newest by `first_seen` is the current one.
        let Some(current) = recs.iter().max_by_key(|r| r.first_seen) else {
            return Lane::Unknown;
        };
        let closed = matches!(current.intent, Intent::Closed { .. });
        match self.spec_verdict(role) {
            SpecVerdict::Blocked => Lane::ClosedOnPurpose,
            // `agentlife stop --soft` leaves registry intent `Closed{exited}` and writes `soft`
            // in the spec, which is the authority (`start.rs`); only with the wake path on.
            SpecVerdict::Startable { soft_stop: true } if self.wake_enabled => Lane::Launchable,
            SpecVerdict::Startable { soft_stop: true } => Lane::ClosedOnPurpose,
            _ if closed => Lane::ClosedOnPurpose,
            _ => Lane::Launchable,
        }
    }

    /// Rule 7. The grant is the implied one of board 216 (CireSnave: "216: yes"): a lane whose spec
    /// the launcher may start from (a soft stop, a crash) is granted; a hard or unclassified stop,
    /// an unusable spec and no spec are not. Nothing is granted while `lazy_enabled` is off. There
    /// is no M5 store behind this (docs/M5-WAKE-DESIGN.md).
    fn granted(&self, role: &str) -> bool {
        self.wake_enabled
            && matches!(self.spec_verdict(role), SpecVerdict::Startable { .. })
            // The grant never exceeds the lane rule: a closed registry intent that no soft stop
            // overrides is not granted even though a spec says `running`.
            && self.lane(role) == Lane::Launchable
    }

    fn running(&self, role: &str) -> bool {
        self.records(role).is_some_and(|recs| {
            recs.iter()
                .any(|r| liveness(r, self.table).0 == Liveness::Running)
        })
    }
}

/// The notifier-only mechanism: it starts nothing. Rule 7 refuses before it can be called; if a
/// later change ever reaches it, the answer is a failure, never a launch.
pub struct NoMechanism;

impl Mechanism for NoMechanism {
    fn start(&self, _role: &str, _model: &str, _signal_id: &str) -> MechResult {
        MechResult::Failed("mechanism-not-built".into())
    }
}

/// The account of the Synapse home (`<home>/account`), read without starting a daemon.
pub fn account_of(home: &Path) -> Result<String, String> {
    synapse::keystore::Keystore::open(home)
        .map(|k| k.account().account)
        .map_err(|e| format!("cannot read the Synapse account in {}: {e}", home.display()))
}

/// `role` as the daemon addresses it: `role@account` (an address that already has an `@` is kept).
pub fn qualify(to: &str, account: &str) -> String {
    if to.contains('@') {
        to.to_string()
    } else {
        format!("{to}@{account}")
    }
}

/// The `/v1/send` request body (the shape `synapsectl send` builds).
pub fn send_body(to_role: &str, message_id: &str, text: &str) -> Value {
    serde_json::json!({
        "to": to_role,
        "message_id": message_id,
        "body_b64": synapsed_client::encode_body(text.as_bytes()),
    })
}

/// Which transport the notify path uses: `notify_transport` in `~/.overmind/lane-restart.json`,
/// `broker` when the file is missing or unreadable (the claude-peers path stays the default
/// until the cutover flips it).
pub fn notify_transport(home: Option<&Path>) -> lane_restart::notify::Transport {
    lane_restart::approvals::configured_transport(home)
}

/// Stall alarms to the PM lane, through the Synapse daemon as [`LAUNCHER_ROLE`].
pub struct SynapseNotify<'a> {
    pub source: &'a SynapseSource,
}

impl Notify for SynapseNotify<'_> {
    fn alarm(&self, text: &str) -> bool {
        let id = format!(
            "launcher-alarm-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            std::process::id()
        );
        self.source.send(ALARM_ROLE, &id, text).is_ok()
    }
}

/// Relays what hosts queued (`lane_restart::notify::enqueue`) through the daemon; returns
/// `(sent, kept)`. A refused message stays queued for the next pass.
pub fn relay_outbox(state_dir: &Path, source: &SynapseSource) -> (usize, usize) {
    lane_restart::notify::drain_outbox(state_dir, &mut |m| {
        source.send(&m.to, &m.message_id, &m.text)
    })
}

/// Stall alarms to the PM lane, over claude-peers.
pub struct PeerNotify<'a> {
    pub state_dir: &'a Path,
}

impl Notify for PeerNotify<'_> {
    fn alarm(&self, text: &str) -> bool {
        lane_restart::notify::notify_role(
            lane_restart::notify::broker_addr(),
            self.state_dir,
            ALARM_ROLE,
            text,
        )
        .is_ok()
    }
}

// ---------------------------------------------------------------------------------------------
// Identity: the instance lock and the caller record.

/// Holds the OS advisory lock for the service's lifetime; the OS releases it on any exit.
pub struct InstanceLock {
    _file: std::fs::File,
}

/// `Ok(None)` when another instance holds it.
pub fn take_instance_lock(state_dir: &Path) -> std::io::Result<Option<InstanceLock>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(state_dir))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// This process's record, or `None` when its start time cannot be read (no record is better
/// than a wrong one).
pub fn own_record() -> Option<LauncherRecord> {
    let pid = std::process::id();
    let id = SysinfoTable.identity_of(pid)?;
    Some(LauncherRecord {
        role: LAUNCHER_ROLE.to_string(),
        pid,
        pid_start_secs: id.start_secs,
    })
}

fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(launcher_record_name(LAUNCHER_ROLE))
}

/// Writes the caller record. Call only while a valid session is held.
pub fn write_caller_record(state_dir: &Path, rec: &LauncherRecord) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(rec).map_err(std::io::Error::other)?;
    crate::atomic::write_atomic(&record_path(state_dir), &bytes)
}

/// Removes the record only if it still names `rec` (this pid and start time): a successor's
/// record is never deleted. Returns whether a file was removed.
pub fn remove_caller_record(state_dir: &Path, rec: &LauncherRecord) -> bool {
    let path = record_path(state_dir);
    let still_mine = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<LauncherRecord>(&b).ok())
        .is_some_and(|on_disk| on_disk == *rec);
    still_mine && std::fs::remove_file(&path).is_ok()
}

#[cfg(test)]
mod tests;
