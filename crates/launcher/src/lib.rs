// SPDX-License-Identifier: MIT OR Apache-2.0
//! The decision engine of the launch-on-message service (`docs/LAUNCHER-SERVICE.md`), implementing
//! OverMind's policy (`ciresnave/OverMind` `docs/LANE-LAUNCH-POLICY.md` @e4c6ae3, sections 2-4).
//!
//! Pure: no network, no process spawn, no clock. The daemon (`SignalSource`/results), the
//! mechanism that starts a lane, the registry and the grant store are traits supplied by the
//! caller, and time is passed in. That is what lets every rule be tested with a stand-in.
//!
//! Crash safety (design, Store): a request row is written as `Intent` and saved **before** the
//! mechanism is called, becomes `Called` with the mapped result **before** it is posted, and is
//! re-posted from the outbox until acknowledged. A row that reached `Intent` is never called again.
//!
//! The model is never taken from a signal: [`Signal`] has no model field, and the mechanism is
//! always asked for [`MODEL`].

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// The only model this engine ever requests (OverMind constraint 1).
pub const MODEL: &str = "sonnet";
/// A repeat of the same `(signal_id, attempt, epoch)` inside this window is a replay.
pub const REPLAY_WINDOW_SECS: i64 = 300;
/// Rows older than this are dropped.
pub const PRUNE_SECS: i64 = 24 * 3600;
const HOUR_SECS: i64 = 3600;
/// Rule 6: spawns per role and overall in the last hour.
pub const RATE_PER_ROLE: usize = 3;
pub const RATE_OVERALL: usize = 12;
/// Rule 5: consecutive spawns without the role having been seen online.
pub const NO_CLAIM_BOUND: u32 = 3;
/// R7: consecutive `already-running` answers without the role seen online.
pub const STALL_STREAK: u32 = 5;
/// The store's schema; a store with a newer one is refused, never rewritten.
pub const SCHEMA: u32 = 1;

/// One signal from Synapse's `/v1/waiting`. Anything else the daemon sends is not read: there is
/// no model, body or token field here to be filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub role: String,
    pub signal_id: String,
    pub attempt: u32,
    /// Synapse's counter bumped by a clear or restart; 0 until Synapse sends one.
    #[serde(default)]
    pub epoch: u64,
}

/// The four outcomes Synapse's `/v1/launch-result` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Started,
    AlreadyRunning,
    Refused,
    Failed,
}

/// The body of `POST /v1/launch-result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchResult {
    pub signal_id: String,
    pub attempt: u32,
    pub role: String,
    pub outcome: Outcome,
    pub reason: String,
}

/// What the mechanism (start one lane by role) reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MechResult {
    Working,
    Started,
    AwaitingDialog,
    AlreadyRunning,
    HeldForMemory,
    LockHeld,
    /// Carries the mechanism's own short code; `model-not-pinned` is kept, anything else is
    /// `spawn-failed`.
    Failed(String),
    InvalidArgument,
    /// No answer within `launch_wait` minus 10 s.
    Timeout,
}

pub trait Mechanism {
    /// Start `role` once, with `model`. Never `--resume`, never Opus: the mechanism enforces the
    /// model pin too; this engine only ever passes [`MODEL`].
    fn start(&self, role: &str, model: &str, signal_id: &str) -> MechResult;
}

/// What the registry and launch spec say about a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Not a known lane: a sender-chosen string is never a launch.
    Unknown,
    /// Registry intent `closed`, or a `suspended`/`stopping` spec: stopped on purpose.
    ClosedOnPurpose,
    Launchable,
}

pub trait Directory {
    fn lane(&self, role: &str) -> Lane;
    /// A standing grant covers "start `role`" (rule 7). `false` until M5 exists.
    fn granted(&self, role: &str) -> bool;
    /// Is the role's session alive right now (the process table, not Synapse)? Used only to
    /// resolve a row left in `Intent` by a crash.
    fn running(&self, role: &str) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Saved before the mechanism was called.
    Intent,
    /// The mechanism answered; the result is recorded and may not yet be posted.
    Called,
    /// A permanent refusal (never re-checked).
    Answered,
    /// A conditional refusal the service re-checks itself.
    Transient,
    /// The condition ended; `launch-clear` is being (re)tried.
    Clearing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub signal_id: String,
    pub attempt: u32,
    pub epoch: u64,
    /// Incremented when the same key arrives again after the replay window.
    pub generation: u32,
    pub role: String,
    pub at: i64,
    pub status: Status,
    pub outcome: Outcome,
    pub reason: String,
    /// The mechanism was called (or was about to be: `Intent`).
    pub called: bool,
    /// The result reached Synapse.
    pub posted: bool,
}

impl Request {
    fn key_eq(&self, s: &Signal) -> bool {
        self.signal_id == s.signal_id && self.attempt == s.attempt && self.epoch == s.epoch
    }

    /// Rules 5 and 6 count lanes started: a row in `Intent` (conservative), a `started` outcome,
    /// or a spawn that failed.
    fn counts_spawn(&self) -> bool {
        self.status == Status::Intent
            || (self.called && (self.outcome == Outcome::Started || self.reason == "spawn-failed"))
    }

    fn result(&self) -> LaunchResult {
        LaunchResult {
            signal_id: self.signal_id.clone(),
            attempt: self.attempt,
            role: self.role.clone(),
            outcome: self.outcome,
            reason: self.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRow {
    pub spawns_since_online: u32,
    pub last_seen_online: Option<i64>,
    pub already_running_streak: u32,
    pub alarmed: bool,
    /// When the latest counted spawn was recorded.
    pub last_spawn_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreData {
    pub schema: u32,
    pub requests: Vec<Request>,
    pub roles: BTreeMap<String, RoleRow>,
}

impl Default for StoreData {
    fn default() -> Self {
        StoreData {
            schema: SCHEMA,
            requests: Vec::new(),
            roles: BTreeMap::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StoreError {
    /// No store and no explicit `init`: never created on the engine's own initiative.
    Missing,
    Unreadable(String),
    Newer(u32),
    Write(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Missing => write!(f, "no store; an operator must initialise it"),
            StoreError::Unreadable(w) => write!(f, "the store cannot be read: {w}"),
            StoreError::Newer(v) => write!(f, "the store is schema {v}, newer than {SCHEMA}"),
            StoreError::Write(w) => write!(f, "the store cannot be written: {w}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub trait StoreIo {
    /// `Ok(None)` when no store exists.
    fn load(&self) -> Result<Option<StoreData>, StoreError>;
    fn save(&self, data: &StoreData) -> Result<(), StoreError>;
}

/// The store as one JSON file, written atomically (temp file, flush, rename).
pub struct FileStore {
    pub path: PathBuf,
}

impl StoreIo for FileStore {
    fn load(&self) -> Result<Option<StoreData>, StoreError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Unreadable(e.to_string())),
        };
        // The schema field is read first so a newer store is refused for what it is.
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::Unreadable(e.to_string()))?;
        let schema = v.get("schema").and_then(|s| s.as_u64()).unwrap_or(0) as u32;
        if schema > SCHEMA {
            return Err(StoreError::Newer(schema));
        }
        serde_json::from_value(v)
            .map(Some)
            .map_err(|e| StoreError::Unreadable(e.to_string()))
    }

    fn save(&self, data: &StoreData) -> Result<(), StoreError> {
        use std::io::Write;
        let bytes =
            serde_json::to_vec_pretty(data).map_err(|e| StoreError::Write(e.to_string()))?;
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::File::create(&tmp).map_err(|e| StoreError::Write(e.to_string()))?;
        f.write_all(&bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| StoreError::Write(e.to_string()))?;
        drop(f);
        std::fs::rename(&tmp, &self.path).map_err(|e| StoreError::Write(e.to_string()))
    }
}

/// Rule 2: `[A-Za-z0-9_-]{1,64}`.
pub fn valid_role(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 64
        && role
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Section 3: the mechanism's answer as Synapse's `(outcome, reason)`, and whether a lane was
/// actually spawned (only a spawn consumes the rule 5/6 budget).
pub fn map_mechanism(r: &MechResult) -> (Outcome, &'static str) {
    match r {
        MechResult::Working | MechResult::Started => (Outcome::Started, "started"),
        MechResult::AwaitingDialog => (Outcome::Started, "awaiting-dialog"),
        MechResult::AlreadyRunning => (Outcome::AlreadyRunning, "already-running"),
        MechResult::HeldForMemory => (Outcome::Failed, "held-for-memory"),
        MechResult::LockHeld => (Outcome::Failed, "lock-held"),
        MechResult::Failed(code) if code == "model-not-pinned" => {
            (Outcome::Failed, "model-not-pinned")
        }
        MechResult::Failed(_) => (Outcome::Failed, "spawn-failed"),
        MechResult::InvalidArgument => (Outcome::Refused, "invalid-argument"),
        MechResult::Timeout => (Outcome::Failed, "mechanism-timeout"),
    }
}

pub struct Engine<'a> {
    data: StoreData,
    io: &'a dyn StoreIo,
    mech: &'a dyn Mechanism,
    dir: &'a dyn Directory,
}

impl<'a> Engine<'a> {
    /// Opens the store. A missing store is refused unless `init` (an explicit operator action);
    /// an unreadable or newer one is always refused (rule 1: fail closed).
    pub fn open(
        io: &'a dyn StoreIo,
        mech: &'a dyn Mechanism,
        dir: &'a dyn Directory,
        init: bool,
    ) -> Result<Self, StoreError> {
        let data = match io.load()? {
            Some(d) => d,
            None if init => {
                let d = StoreData::default();
                io.save(&d)?;
                d
            }
            None => return Err(StoreError::Missing),
        };
        Ok(Engine {
            data,
            io,
            mech,
            dir,
        })
    }

    pub fn data(&self) -> &StoreData {
        &self.data
    }

    fn save(&self) -> Result<(), StoreError> {
        self.io.save(&self.data)
    }

    fn store_failed(sig: &Signal) -> LaunchResult {
        LaunchResult {
            signal_id: sig.signal_id.clone(),
            attempt: sig.attempt,
            role: sig.role.clone(),
            outcome: Outcome::Failed,
            reason: "store-unavailable".into(),
        }
    }

    fn prune(&mut self, now: i64) {
        self.data.requests.retain(|r| now - r.at < PRUNE_SECS);
    }

    fn spawns_last_hour(&self, now: i64, role: Option<&str>) -> usize {
        self.data
            .requests
            .iter()
            .filter(|r| now - r.at < HOUR_SECS && r.counts_spawn())
            .filter(|r| role.is_none_or(|x| r.role == x))
            .count()
    }

    /// Decides one signal, in the order of OverMind's section 2, and returns the result to post.
    /// The result is also kept in the outbox until [`Engine::mark_posted`].
    pub fn handle(&mut self, sig: &Signal, now: i64) -> LaunchResult {
        self.prune(now);
        // The row is built once; `finish` stores it and saves, failing closed on a write error.
        let generation = self
            .data
            .requests
            .iter()
            .filter(|r| r.key_eq(sig))
            .map(|r| r.generation + 1)
            .max()
            .unwrap_or(0);
        let mut row = Request {
            signal_id: sig.signal_id.clone(),
            attempt: sig.attempt,
            epoch: sig.epoch,
            generation,
            role: sig.role.clone(),
            at: now,
            status: Status::Answered,
            outcome: Outcome::Refused,
            reason: String::new(),
            called: false,
            posted: false,
        };

        // Rule 2.
        if !valid_role(&sig.role) {
            return self.refuse(row, "invalid-role", Status::Answered, sig);
        }
        // Rule 3 (with the intent rule from the design: a stop on purpose is terminal).
        match self.dir.lane(&sig.role) {
            Lane::Unknown => return self.refuse(row, "unknown-role", Status::Answered, sig),
            Lane::ClosedOnPurpose => {
                return self.refuse(row, "closed-on-purpose", Status::Answered, sig)
            }
            Lane::Launchable => {}
        }
        // Rule 4: a replay answers the recorded result and does nothing else. A stored refusal
        // (called == false) is never replayed: it is re-evaluated below.
        if let Some(prev) = self
            .data
            .requests
            .iter()
            .filter(|r| r.key_eq(sig) && r.called && now - r.at < REPLAY_WINDOW_SECS)
            .max_by_key(|r| r.generation)
        {
            return if prev.status == Status::Intent {
                LaunchResult {
                    outcome: Outcome::Failed,
                    reason: "in-flight".into(),
                    ..prev.result()
                }
            } else {
                prev.result()
            };
        }
        // Rule 5.
        let since = self
            .data
            .roles
            .get(&sig.role)
            .map_or(0, |r| r.spawns_since_online);
        if since >= NO_CLAIM_BOUND {
            return self.refuse(row, "no-claim", Status::Transient, sig);
        }
        // Rule 6.
        if self.spawns_last_hour(now, Some(&sig.role)) >= RATE_PER_ROLE
            || self.spawns_last_hour(now, None) >= RATE_OVERALL
        {
            return self.refuse(row, "rate", Status::Transient, sig);
        }
        // Rule 7.
        if !self.dir.granted(&sig.role) {
            return self.refuse(row, "no-grant", Status::Transient, sig);
        }
        // Rule 8: intent saved first, then the one mechanism call, then the result saved.
        row.status = Status::Intent;
        row.called = true;
        row.outcome = Outcome::Failed;
        row.reason = "in-flight".into();
        self.data.requests.push(row);
        let idx = self.data.requests.len() - 1;
        if self.save().is_err() {
            self.data.requests.pop();
            return Self::store_failed(sig);
        }
        let answer = self.mech.start(&sig.role, MODEL, &sig.signal_id);
        let (outcome, reason) = map_mechanism(&answer);
        {
            let r = &mut self.data.requests[idx];
            r.status = Status::Called;
            r.outcome = outcome;
            r.reason = reason.to_string();
        }
        self.note_role(&sig.role, &answer, now);
        let result = self.data.requests[idx].result();
        // A failed save leaves the row in `Intent` on disk: the call is not repeated (recover()).
        let _ = self.save();
        result
    }

    fn refuse(
        &mut self,
        mut row: Request,
        reason: &str,
        status: Status,
        sig: &Signal,
    ) -> LaunchResult {
        row.status = status;
        row.outcome = Outcome::Refused;
        row.reason = reason.into();
        let result = row.result();
        self.data.requests.push(row);
        if self.save().is_err() {
            self.data.requests.pop();
            return Self::store_failed(sig);
        }
        result
    }

    fn note_role(&mut self, role: &str, answer: &MechResult, now: i64) {
        let r = self.data.roles.entry(role.to_string()).or_default();
        match answer {
            MechResult::Working | MechResult::Started | MechResult::AwaitingDialog => {
                r.spawns_since_online += 1;
                r.already_running_streak = 0;
                r.last_spawn_at = Some(now);
            }
            MechResult::AlreadyRunning => r.already_running_streak += 1,
            MechResult::Failed(c) if c != "model-not-pinned" => r.last_spawn_at = Some(now),
            _ => {}
        }
    }

    /// Every result not yet acknowledged by Synapse (refusals included: rule 2 of section 4).
    pub fn outbox(&self) -> Vec<LaunchResult> {
        self.data
            .requests
            .iter()
            .filter(|r| !r.posted && r.status != Status::Intent)
            .map(Request::result)
            .collect()
    }

    /// Synapse acknowledged this result (a 2xx, or a duplicate reply).
    pub fn mark_posted(&mut self, signal_id: &str, attempt: u32) -> Result<(), StoreError> {
        for r in self
            .data
            .requests
            .iter_mut()
            .filter(|r| r.signal_id == signal_id && r.attempt == attempt)
        {
            r.posted = true;
        }
        self.save()
    }

    /// After a restart: a row left in `Intent` (a crash between the save and the answer) is
    /// resolved from the process table and never called again.
    pub fn recover(&mut self, now: i64) -> Result<usize, StoreError> {
        let mut n = 0;
        for i in 0..self.data.requests.len() {
            if self.data.requests[i].status != Status::Intent {
                continue;
            }
            let role = self.data.requests[i].role.clone();
            let up = self.dir.running(&role);
            let r = &mut self.data.requests[i];
            r.status = Status::Called;
            if up {
                r.outcome = Outcome::Started;
                r.reason = "started".into();
            } else {
                r.outcome = Outcome::Failed;
                r.reason = "spawn-failed".into();
            }
            let e = self.data.roles.entry(role).or_default();
            if up {
                e.spawns_since_online += 1;
            }
            e.last_spawn_at = Some(now);
            n += 1;
        }
        self.save()?;
        Ok(n)
    }

    /// A mechanism result that arrived after `mechanism-timeout`: the row is updated so the spawn
    /// counts toward rules 5 and 6.
    pub fn late_result(
        &mut self,
        sig: &Signal,
        answer: &MechResult,
        now: i64,
    ) -> Result<bool, StoreError> {
        let (outcome, reason) = map_mechanism(answer);
        let found = self
            .data
            .requests
            .iter_mut()
            .filter(|r| r.key_eq(sig) && r.reason == "mechanism-timeout")
            .max_by_key(|r| r.generation);
        let Some(r) = found else { return Ok(false) };
        r.outcome = outcome;
        r.reason = reason.into();
        r.posted = false;
        let role = r.role.clone();
        self.note_role(&role, answer, now);
        self.save()?;
        Ok(true)
    }

    /// R7, once per poll cycle: `online` is the set of roles Synapse's `/v1/list` shows online.
    /// A role seen online resets its consecutive-spawn counter, its streak and its alarm; a
    /// `no-claim` refusal for it becomes clearable.
    pub fn observe_online(
        &mut self,
        online: &BTreeSet<String>,
        now: i64,
    ) -> Result<(), StoreError> {
        for role in online {
            if let Some(r) = self.data.roles.get_mut(role) {
                r.spawns_since_online = 0;
                r.already_running_streak = 0;
                r.alarmed = false;
                r.last_seen_online = Some(now);
            }
        }
        self.save()
    }

    /// `launch-stalled: <role> <reason>`, once per streak. `launch_wait` is Synapse's (120 s,
    /// unmeasured).
    pub fn alarms(&mut self, now: i64, launch_wait: i64) -> Vec<String> {
        let mut out = Vec::new();
        for (role, r) in self.data.roles.iter_mut() {
            if r.alarmed {
                continue;
            }
            let never_online_since_spawn = match (r.last_spawn_at, r.last_seen_online) {
                (Some(sp), Some(on)) => on < sp,
                (Some(_), None) => true,
                (None, _) => false,
            };
            let stale = r.last_spawn_at.is_some_and(|sp| now - sp > 2 * launch_wait);
            let reason = if !never_online_since_spawn {
                None
            } else if r.spawns_since_online >= NO_CLAIM_BOUND {
                // The rule 5 refusal and a stale `started` share one cause: reported once.
                Some("no-claim")
            } else if r.already_running_streak >= STALL_STREAK {
                Some("already-running-streak")
            } else if r.spawns_since_online >= 1 && stale {
                Some("started-never-claimed")
            } else {
                None
            };
            if let Some(why) = reason {
                r.alarmed = true;
                out.push(format!("launch-stalled: {role} {why}"));
            }
        }
        let _ = self.save();
        out
    }

    /// Conditional refusals whose condition has ended (rule 3 of section 4): their rows become
    /// `Clearing` and the roles are returned for `POST /v1/launch-clear`. Rows already `Clearing`
    /// are returned again, so a failed clear is retried every cycle.
    pub fn recheck(&mut self, now: i64) -> Vec<String> {
        let mut clear: BTreeSet<String> = BTreeSet::new();
        for i in 0..self.data.requests.len() {
            let (status, reason, role) = {
                let r = &self.data.requests[i];
                (r.status, r.reason.clone(), r.role.clone())
            };
            match status {
                Status::Clearing => {
                    clear.insert(role);
                }
                Status::Transient => {
                    let over = match reason.as_str() {
                        "rate" => {
                            self.spawns_last_hour(now, Some(&role)) < RATE_PER_ROLE
                                && self.spawns_last_hour(now, None) < RATE_OVERALL
                        }
                        "no-grant" => self.dir.granted(&role),
                        "no-claim" => self
                            .data
                            .roles
                            .get(&role)
                            .is_none_or(|r| r.spawns_since_online < NO_CLAIM_BOUND),
                        _ => false,
                    };
                    if over {
                        self.data.requests[i].status = Status::Clearing;
                        clear.insert(role);
                    }
                }
                _ => {}
            }
        }
        let _ = self.save();
        clear.into_iter().collect()
    }

    /// `launch-clear` succeeded for `role`: only now are its `Clearing` rows deleted.
    pub fn cleared(&mut self, role: &str) -> Result<(), StoreError> {
        self.data
            .requests
            .retain(|r| !(r.role == role && r.status == Status::Clearing));
        self.save()
    }
}

#[cfg(test)]
mod tests;
