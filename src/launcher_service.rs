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
use crate::spec_launch;
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

/// The `outcome` string of `POST /v1/launch-result`. Synapse's daemon accepts EXACTLY these four
/// (anything else is a 400 `malformed`); `dry-run` is never posted.
pub fn outcome_wire(o: Outcome) -> &'static str {
    match o {
        Outcome::Started => "started",
        Outcome::AlreadyRunning => "already-running",
        Outcome::Refused => "refused",
        Outcome::Failed => "failed",
    }
}

/// The real daemon, as role [`LAUNCHER_ROLE`].
pub struct SynapseSource {
    daemon: synapsed_client::Daemon,
    notes: std::cell::RefCell<Vec<String>>,
}

impl SynapseSource {
    /// Finds this home's daemon (starting `synapsed` if nothing answers, as every Synapse client
    /// does) and proves it.
    pub fn connect(home: &Path) -> Result<Self, String> {
        synapsed_client::Daemon::connect(home)
            .map(|daemon| SynapseSource {
                daemon,
                notes: std::cell::RefCell::new(Vec::new()),
            })
            .map_err(|e| e.to_string())
    }

    /// Holds a valid session for the role: the cached one, else a claim. The caller record is
    /// written only after this succeeds.
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
        let outcome = outcome_wire(r.outcome);
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
}

impl RegistryDirectory<'_> {
    /// The spec stops the launcher: it records a stop, or it exists but cannot be used (corrupt,
    /// incomplete, another role's, a newer schema). Only a spec that is genuinely absent leaves
    /// the decision to the registry (`lane-restart start` itself refuses a role with no spec).
    /// An unusable spec fails closed because it may be hiding a deliberate stop.
    fn spec_blocks(&self, role: &str) -> bool {
        match lane_restart::spec::read(self.lane_state_dir, role) {
            Err(lane_restart::spec::SpecError::NotFound(_)) => false,
            _ => spec_launch::load(self.lane_state_dir, role).is_none_or(|s| s.suspended),
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
        if closed || self.spec_blocks(role) {
            Lane::ClosedOnPurpose
        } else {
            Lane::Launchable
        }
    }

    /// No standing grant exists until M5 (board 216).
    fn granted(&self, _role: &str) -> bool {
        false
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
