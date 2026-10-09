// SPDX-License-Identifier: MIT OR Apache-2.0
//! `park` / `stop` of a **running** agent: ask it to wrap up, wait until it provably has, then stop
//! exactly that process. The first code in agentlife that ends a process, so every step is written
//! to fail toward "leave it running" (DESIGN.md §3; DESIGN-REVISION-2 §3, §4).
//!
//! The order, and why:
//!
//! 1. **Authorize** (`control::authorize`), refuse **self** (a lane stopping its own process is a
//!    separate, riskier change), and require `--confirm <name>` for a pinned agent.
//! 2. **Verify what would be stopped**: the agent's last session must carry a process start time, so
//!    "this pid" means "this process" and not a recycled pid.
//! 3. **Dry run unless `--yes`**: print the plan and do nothing (lane-restart's rule for acting on a
//!    lane that is not yourself).
//! 4. **Ask the lane to wrap up** over the claude-peers broker, to the peer whose helper process is a
//!    child of this agent's `claude` (never by cwd or by an id that rotates). If it cannot be asked,
//!    **nothing is changed**.
//! 5. **Wait for readiness** (`readiness::check`, all conditions, evidence newer than the request).
//!    On timeout the agent is **left running** and the blockers are reported.
//! 6. **Record the intent, then stop**: `Closed` and a `closed` journal entry are written *before*
//!    the process is touched, the identity is re-read immediately before the kill, and if the kill
//!    fails or the process does not go, the intent is **put back** and the failure journalled.

use crate::caller::Caller;
use crate::clock::Clock;
use crate::config::Config;
use crate::control::{authorize, by_label, Action};
use crate::identity::ProcessIdentity;
use crate::identity::ProcessTable;
use crate::journal::Journal;
use crate::list::{liveness, Liveness};
use crate::marks::pin_kind;
use crate::peers::{peers_of_claude, Messenger, Peer};
use crate::readiness::{self, Evidence, LaneStateView, ProcTree};
use crate::registry::{AgentId, AgentRecord, ClosedHow, Intent, Registry};
use chrono::{DateTime, Utc};
use lane_state::claude_proc::ParentProcess;
use serde_json::json;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillError {
    /// Something else now holds the pid (it started at a different time).
    Mismatch {
        expected_start: u64,
        actual_start: u64,
    },
    NotRunning,
    Failed(String),
}

/// Ending one process, as a trait so the sequence is testable without killing anything.
pub trait Terminator {
    /// Re-reads the process **now**, refuses if it is not the one recorded, and terminates it.
    fn kill_verified(&self, expected: &ProcessIdentity) -> Result<(), KillError>;
    fn is_gone(&self, expected: &ProcessIdentity) -> bool;
}

/// The facts read from the filesystem, as a trait so a test can script them.
pub trait Facts {
    fn lane_state(&self, session_id: &str) -> Option<LaneStateView>;
    /// The candidate HANDOFF paths, and the newest one that exists.
    fn handoff(&self, rec: &AgentRecord) -> (Vec<PathBuf>, Option<(PathBuf, DateTime<Utc>)>);
}

pub struct FsFacts<'a> {
    pub lane_state_dir: &'a Path,
}

impl Facts for FsFacts<'_> {
    fn lane_state(&self, session_id: &str) -> Option<LaneStateView> {
        readiness::read_lane_state(self.lane_state_dir, session_id)
    }

    fn handoff(&self, rec: &AgentRecord) -> (Vec<PathBuf>, Option<(PathBuf, DateTime<Utc>)>) {
        let candidates = readiness::handoff_candidates(rec);
        let newest = readiness::newest_mtime(&candidates);
        (candidates, newest)
    }
}

pub struct SysinfoTerminator;

impl Terminator for SysinfoTerminator {
    fn kill_verified(&self, expected: &ProcessIdentity) -> Result<(), KillError> {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let pid = Pid::from_u32(expected.pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let proc_ = sys.process(pid).ok_or(KillError::NotRunning)?;
        if crate::identity::process_is_dead(proc_) {
            return Err(KillError::NotRunning);
        }
        if proc_.start_time() != expected.start_secs {
            return Err(KillError::Mismatch {
                expected_start: expected.start_secs,
                actual_start: proc_.start_time(),
            });
        }
        if proc_.kill() {
            Ok(())
        } else {
            Err(KillError::Failed(
                "the operating system refused to end the process".into(),
            ))
        }
    }

    fn is_gone(&self, expected: &ProcessIdentity) -> bool {
        use crate::identity::{check, Match, SysinfoTable};
        check(&SysinfoTable, expected) != Match::Same
    }
}

pub struct Deps<'a> {
    pub registry: &'a Registry,
    pub journal: &'a Journal,
    pub cfg: &'a Config,
    pub table: &'a dyn ProcessTable,
    pub messenger: &'a dyn Messenger,
    pub parents: &'a dyn ParentProcess,
    pub tree: &'a dyn ProcTree,
    pub facts: &'a dyn Facts,
    pub terminator: &'a dyn Terminator,
    pub clock: &'a dyn Clock,
    pub sleep: &'a dyn Fn(Duration),
}

pub struct Request<'a> {
    pub target: &'a AgentId,
    pub how: ClosedHow,
    pub confirm: Option<&'a str>,
    pub yes: bool,
    pub timeout: Duration,
    pub poll: Duration,
    pub caller: &'a Caller,
    pub caller_rec: Option<&'a AgentRecord>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Without `--yes`: what would happen. Nothing was done.
    DryRun(Vec<String>),
    Stopped {
        pid: u32,
        waited: Duration,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum DownError {
    NotFound(AgentId),
    Denied(String),
    /// A lane stopping its own process is not supported yet.
    IsSelf,
    AlreadyClosed,
    /// Its process is not running: use the registry-only path.
    NotRunning,
    /// Alive, but the record cannot prove which process it is.
    CannotVerify(String),
    NeedsConfirmation {
        expected: String,
    },
    /// The lane could not be asked; nothing was changed.
    CannotAsk(String),
    /// It never became ready; it was left running.
    NotReady {
        blockers: Vec<String>,
        waited: Duration,
    },
    /// The pid is held by a different process now; nothing was stopped.
    IdentityChanged(String),
    /// The stop failed; the intent was put back.
    StopFailed(String),
    Registry(String),
}

impl fmt::Display for DownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownError::NotFound(id) => write!(f, "no agent {id}"),
            DownError::Denied(why) => write!(f, "refused: {why}"),
            DownError::IsSelf => write!(
                f,
                "a lane cannot stop its own process yet; ask the PM or a person to stop it"
            ),
            DownError::AlreadyClosed => write!(f, "the agent is already closed"),
            DownError::NotRunning => write!(f, "the agent is not running"),
            DownError::CannotVerify(why) => write!(f, "cannot tell which process this is, so nothing was stopped: {why}"),
            DownError::NeedsConfirmation { expected } => write!(
                f,
                "this agent is pinned; type its name to confirm: --confirm {expected}"
            ),
            DownError::CannotAsk(why) => write!(
                f,
                "could not ask the lane to wrap up, so nothing was changed: {why}"
            ),
            DownError::NotReady { blockers, waited } => write!(
                f,
                "the agent did not become ready to stop within {}s, so it was left running:\n  - {}",
                waited.as_secs(),
                blockers.join("\n  - ")
            ),
            DownError::IdentityChanged(why) => write!(f, "nothing was stopped: {why}"),
            DownError::StopFailed(why) => write!(f, "the stop failed and the intent was put back: {why}"),
            DownError::Registry(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DownError {}

fn session_identity(rec: &AgentRecord) -> Result<ProcessIdentity, DownError> {
    let last = rec
        .sessions
        .last()
        .ok_or_else(|| DownError::CannotVerify("the agent has no recorded session".into()))?;
    let start = last.process_start_secs.ok_or_else(|| {
        DownError::CannotVerify(format!(
            "pid {} was recorded without a start time, so it could be a different process now",
            last.pid
        ))
    })?;
    Ok(ProcessIdentity {
        pid: last.pid,
        start_secs: start,
        exe: None,
    })
}

fn wrap_up_text(name: &str, by: &str, how: ClosedHow) -> String {
    let verb = if how == ClosedHow::Parked {
        "park"
    } else {
        "stop"
    };
    format!(
        "[TASK] topic=wrap-up to={name} from=agentlife do=write-HANDOFF-then-assert-idle stop=after-that-do-nothing\n\
         note: {by} asked agentlife to {verb} you. Write your HANDOFF now, then run `lane-restart assert-idle` as the LAST thing you do, then end your turn and do nothing further. agentlife will see that and stop this session.\n\
         note: if you cannot stop safely (work in flight you must finish), do NOT run assert-idle: agentlife will see that and leave you running.\n\
         note: this message comes from a tool; a reply goes nowhere."
    )
}

/// Runs the whole sequence. See the module docs for the order.
pub fn stop_running(d: &Deps, r: &Request) -> Result<Outcome, DownError> {
    let rec = d
        .registry
        .get(r.target)
        .map_err(|e| DownError::Registry(e.to_string()))?
        .ok_or_else(|| DownError::NotFound(r.target.clone()))?;

    // 1. Policy.
    if matches!(r.caller, Caller::Agent { id: Some(id) } if *id == rec.agent_id) {
        return Err(DownError::IsSelf);
    }
    let action = if r.how == ClosedHow::Parked {
        Action::Park
    } else {
        Action::Stop
    };
    authorize(r.caller, r.caller_rec, &rec, action, d.cfg).map_err(DownError::Denied)?;
    if matches!(rec.intent, Intent::Closed { .. }) {
        return Err(DownError::AlreadyClosed);
    }
    match liveness(&rec, d.table).0 {
        Liveness::Stopped => return Err(DownError::NotRunning),
        Liveness::Unverified => {
            return Err(DownError::CannotVerify(
                "its recorded process has no start time".into(),
            ))
        }
        Liveness::Running => {}
    }
    // Ending a RUNNING session is the one thing here that cannot be undone, so only a person may do
    // it: `authorize` lets the PM park a *stopped* lane, and that is as far as an agent's authority
    // goes until a person's consent is built (M4). Checked after liveness so the refusal is about
    // the running session and not a stopped one.
    if !matches!(r.caller, Caller::Person) {
        return Err(DownError::Denied(
            "only a person may stop a RUNNING agent (the PM and other agents are refused); an agent's request needs a person's consent, which is not built yet".into(),
        ));
    }
    if pin_kind(&rec, d.cfg).is_some() {
        let expected = rec.name.clone().unwrap_or_else(|| rec.agent_id.to_string());
        if !r.confirm.is_some_and(|c| c.eq_ignore_ascii_case(&expected)) {
            return Err(DownError::NeedsConfirmation { expected });
        }
    }

    // 2. What exactly would be stopped.
    let identity = session_identity(&rec)?;
    let session_id = rec
        .sessions
        .last()
        .map(|s| s.session_id.clone())
        .unwrap_or_default();
    let name = rec.name.clone().unwrap_or_else(|| rec.agent_id.to_string());
    let by = by_label(r.caller, r.caller_rec, d.cfg);

    // 3. The peers that would be asked (a read), so the plan can name them.
    let peers_result = d.messenger.peers();
    let peers: Vec<Peer> = match &peers_result {
        Ok(all) => peers_of_claude(all, d.parents, identity.pid)
            .into_iter()
            .cloned()
            .collect(),
        Err(_) => Vec::new(),
    };
    if !r.yes {
        let mut plan = vec![format!(
            "would stop {name} ({}), pid {} (started {}), after it wraps up",
            rec.agent_id, identity.pid, identity.start_secs
        )];
        plan.push(match &peers_result {
            Ok(_) if peers.is_empty() => {
                "it has NO claude-peers session below its process, so it cannot be asked: this would fail"
                    .to_string()
            }
            Ok(_) => format!(
                "would ask peer(s) {} to write HANDOFF and run assert-idle, then wait up to {}s",
                peers.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", "),
                r.timeout.as_secs()
            ),
            Err(e) => format!("the claude-peers broker cannot be reached ({e}): this would fail"),
        });
        plan.push(format!(
            "would then record it {} and end exactly that process",
            if r.how == ClosedHow::Parked {
                "parked"
            } else {
                "stopped"
            }
        ));
        plan.push("nothing was done; run again with --yes to do it".to_string());
        return Ok(Outcome::DryRun(plan));
    }

    // 4. Ask.
    if let Err(e) = &peers_result {
        return Err(DownError::CannotAsk(e.clone()));
    }
    if peers.is_empty() {
        return Err(DownError::CannotAsk(
            "no claude-peers session was found below this agent's process".into(),
        ));
    }
    let asked_at = d.clock.now();
    let text = wrap_up_text(&name, &by, r.how);
    let mut reached = Vec::new();
    let mut errors = Vec::new();
    for p in &peers {
        match d.messenger.send(&p.id, &text) {
            Ok(()) => reached.push(p.id.clone()),
            Err(e) => errors.push(format!("{}: {e}", p.id)),
        }
    }
    if reached.is_empty() {
        return Err(DownError::CannotAsk(errors.join("; ")));
    }
    let _ = d.journal.append(
        "down-requested",
        Some(rec.agent_id.as_str()),
        json!({"by": by, "how": r.how, "peers": reached, "timeout_secs": r.timeout.as_secs(), "pid": identity.pid}),
    );

    // 5. Wait until it is provably ready.
    let waited = loop {
        let waited = (d.clock.now() - asked_at).to_std().unwrap_or_default();
        let state = d.facts.lane_state(&session_id);
        let (looked_in, handoff) = d.facts.handoff(&rec);
        let shells = readiness::shells_below(d.tree, identity.pid);
        let verdict = readiness::check(&Evidence {
            state: state.as_ref(),
            handoff: handoff.as_ref(),
            handoff_looked_in: &looked_in,
            shells: &shells,
            asked_at,
        });
        if verdict.ready {
            break waited;
        }
        if waited >= r.timeout {
            let _ = d.journal.append(
                "down-timeout",
                Some(rec.agent_id.as_str()),
                json!({"by": by, "blockers": verdict.blockers, "waited_secs": waited.as_secs()}),
            );
            return Err(DownError::NotReady {
                blockers: verdict.blockers,
                waited,
            });
        }
        (d.sleep)(r.poll);
    };

    // 6. Record the intent FIRST, then stop exactly that process.
    let now = d.clock.now();
    let mut applied = false;
    d.registry
        .update(&rec.agent_id, |x| {
            if !matches!(x.intent, Intent::Closed { .. }) {
                x.intent = Intent::Closed {
                    how: r.how,
                    by: by.clone(),
                    at: now,
                };
                applied = true;
            }
        })
        .map_err(|e| DownError::Registry(e.to_string()))?;
    if !applied {
        return Err(DownError::AlreadyClosed);
    }
    let _ = d.journal.append(
        "closed",
        Some(rec.agent_id.as_str()),
        json!({"how": r.how, "by": by, "pid": identity.pid, "graceful": true}),
    );

    let put_back = |why: &str| {
        let _ = d.registry.update(&rec.agent_id, |x| {
            if matches!(&x.intent, Intent::Closed { by: b, at: a, .. } if *b == by && *a == now) {
                x.intent = Intent::Wanted;
            }
        });
        let _ = d.journal.append(
            "close-failed",
            Some(rec.agent_id.as_str()),
            json!({"why": why, "pid": identity.pid}),
        );
    };

    match d.terminator.kill_verified(&identity) {
        Ok(()) => {}
        Err(KillError::Mismatch {
            expected_start,
            actual_start,
        }) => {
            let why = format!(
                "pid {} now belongs to a different process (started {actual_start}, expected {expected_start})",
                identity.pid
            );
            put_back(&why);
            return Err(DownError::IdentityChanged(why));
        }
        Err(KillError::NotRunning) => {
            // It ended by itself in the meantime: that is the outcome we wanted.
            let _ = d.journal.append(
                "stopped",
                Some(rec.agent_id.as_str()),
                json!({"pid": identity.pid, "already_gone": true}),
            );
            return Ok(Outcome::Stopped {
                pid: identity.pid,
                waited,
            });
        }
        Err(KillError::Failed(why)) => {
            put_back(&why);
            return Err(DownError::StopFailed(why));
        }
    }
    let mut gone = false;
    for _ in 0..20 {
        if d.terminator.is_gone(&identity) {
            gone = true;
            break;
        }
        (d.sleep)(Duration::from_millis(250));
    }
    if !gone {
        let why = format!(
            "pid {} was ended but is still running after 5 s",
            identity.pid
        );
        put_back(&why);
        return Err(DownError::StopFailed(why));
    }
    let _ = d.journal.append(
        "stopped",
        Some(rec.agent_id.as_str()),
        json!({"pid": identity.pid, "waited_secs": waited.as_secs()}),
    );
    Ok(Outcome::Stopped {
        pid: identity.pid,
        waited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::registry::Session;
    use chrono::{Duration as CDuration, TimeZone};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 3, 0, 0).unwrap()
    }

    struct TClock(Mutex<DateTime<Utc>>);
    impl Clock for TClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }
    impl TClock {
        fn advance(&self, d: Duration) {
            let mut g = self.0.lock().unwrap();
            *g += CDuration::from_std(d).unwrap();
        }
    }

    struct Alive(Vec<(u32, u64)>);
    impl ProcessTable for Alive {
        fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
            self.0
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|(p, s)| ProcessIdentity {
                    pid: *p,
                    start_secs: *s,
                    exe: None,
                })
        }
    }

    struct Parents(HashMap<u32, (u32, String)>);
    impl ParentProcess for Parents {
        fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }
        fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
            None
        }
    }

    struct Tree(Vec<(u32, String)>);
    impl ProcTree for Tree {
        fn descendants(&self, _: u32) -> Vec<(u32, String)> {
            self.0.clone()
        }
    }

    struct Msg {
        peers: Result<Vec<Peer>, String>,
        sent: RefCell<Vec<(String, String)>>,
        send_fails: bool,
    }
    impl Messenger for Msg {
        fn peers(&self) -> Result<Vec<Peer>, String> {
            self.peers.clone()
        }
        fn send(&self, to: &str, text: &str) -> Result<(), String> {
            if self.send_fails {
                return Err("peer vanished".into());
            }
            self.sent
                .borrow_mut()
                .push((to.to_string(), text.to_string()));
            Ok(())
        }
    }

    /// Readiness that appears after the Nth poll (or never).
    struct Facts_ {
        clock: Arc<TClock>,
        ready_after_polls: Option<u32>,
        polls: RefCell<u32>,
    }
    impl Facts for Facts_ {
        fn lane_state(&self, session_id: &str) -> Option<LaneStateView> {
            *self.polls.borrow_mut() += 1;
            let ready = self
                .ready_after_polls
                .is_some_and(|n| *self.polls.borrow() > n);
            Some(LaneStateView {
                session_id: session_id.to_string(),
                busy: Some(!ready),
                subagents_running: Some(0),
                no_background_shells: Some(ready),
                updated_at: self.clock.now(),
                updated_by_event: None,
            })
        }
        fn handoff(&self, _: &AgentRecord) -> (Vec<PathBuf>, Option<(PathBuf, DateTime<Utc>)>) {
            let ready = self
                .ready_after_polls
                .is_some_and(|n| *self.polls.borrow() > n);
            let p = PathBuf::from("C:/x/HANDOFF.md");
            (vec![p.clone()], ready.then(|| (p, self.clock.now())))
        }
    }

    struct Term {
        log: Arc<Mutex<Vec<String>>>,
        kill: Result<(), KillError>,
        gone_after_checks: u32,
        checks: RefCell<u32>,
    }
    impl Terminator for Term {
        fn kill_verified(&self, e: &ProcessIdentity) -> Result<(), KillError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("kill pid {} start {}", e.pid, e.start_secs));
            self.kill.clone()
        }
        fn is_gone(&self, _: &ProcessIdentity) -> bool {
            *self.checks.borrow_mut() += 1;
            *self.checks.borrow() > self.gone_after_checks
        }
    }

    fn target() -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new("a-target").unwrap(), "C:/x", t0());
        r.name = Some("target".into());
        r.sessions.push(Session {
            session_id: "sess-t".into(),
            pid: 100,
            process_start_secs: Some(5000),
            started_at: t0(),
            ended_at: None,
            end_reason: None,
        });
        r
    }

    fn pm_record() -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new("a-pm").unwrap(), "C:/Projects", t0());
        r.name = Some("PM".into());
        r.role = Some("pm".into());
        r
    }

    struct Rig {
        _d: tempfile::TempDir,
        registry: Registry,
        journal: Journal,
        clock: Arc<TClock>,
        log: Arc<Mutex<Vec<String>>>,
    }

    fn rig(records: &[AgentRecord]) -> Rig {
        let d = tempfile::tempdir().unwrap();
        let rig = Rig {
            registry: Registry::new(d.path().join("agents")),
            journal: Journal::new(d.path().join("journal"), Arc::new(SystemClock)),
            clock: Arc::new(TClock(Mutex::new(t0() + CDuration::minutes(5)))),
            log: Arc::new(Mutex::new(Vec::new())),
            _d: d,
        };
        for r in records {
            rig.registry.create(r).unwrap();
        }
        rig
    }

    struct Scenario {
        peers: Result<Vec<Peer>, String>,
        send_fails: bool,
        ready_after_polls: Option<u32>,
        shells: Vec<(u32, String)>,
        kill: Result<(), KillError>,
        gone_after_checks: u32,
        alive: Vec<(u32, u64)>,
    }

    impl Default for Scenario {
        fn default() -> Self {
            Scenario {
                peers: Ok(vec![Peer {
                    id: "peer-t".into(),
                    pid: 501,
                    cwd: "C:/x".into(),
                }]),
                send_fails: false,
                ready_after_polls: Some(2),
                shells: vec![],
                kill: Ok(()),
                gone_after_checks: 1,
                alive: vec![(100, 5000)],
            }
        }
    }

    struct Ran {
        result: Result<Outcome, DownError>,
        sent: Vec<(String, String)>,
        kills: Vec<String>,
    }

    #[allow(clippy::too_many_arguments)]
    fn go(
        rig: &Rig,
        sc: Scenario,
        caller: &Caller,
        caller_rec: Option<&AgentRecord>,
        yes: bool,
        confirm: Option<&str>,
        how: ClosedHow,
        timeout_secs: u64,
    ) -> Ran {
        let cfg = Config::default();
        let table = Alive(sc.alive);
        let msg = Msg {
            peers: sc.peers,
            sent: RefCell::new(vec![]),
            send_fails: sc.send_fails,
        };
        let parents = Parents(HashMap::from([(501, (100, "claude.exe".to_string()))]));
        let tree = Tree(sc.shells);
        let facts = Facts_ {
            clock: rig.clock.clone(),
            ready_after_polls: sc.ready_after_polls,
            polls: RefCell::new(0),
        };
        let term = Term {
            log: rig.log.clone(),
            kill: sc.kill,
            gone_after_checks: sc.gone_after_checks,
            checks: RefCell::new(0),
        };
        let clock = rig.clock.clone();
        let sleeper = move |d: Duration| clock.advance(d);
        let id = AgentId::new("a-target").unwrap();
        let result = stop_running(
            &Deps {
                registry: &rig.registry,
                journal: &rig.journal,
                cfg: &cfg,
                table: &table,
                messenger: &msg,
                parents: &parents,
                tree: &tree,
                facts: &facts,
                terminator: &term,
                clock: &*rig.clock,
                sleep: &sleeper,
            },
            &Request {
                target: &id,
                how,
                confirm,
                yes,
                timeout: Duration::from_secs(timeout_secs),
                poll: Duration::from_secs(2),
                caller,
                caller_rec,
            },
        );
        let sent = msg.sent.borrow().clone();
        let kills = rig.log.lock().unwrap().clone();
        Ran {
            result,
            sent,
            kills,
        }
    }

    /// A person at a terminal: the only caller allowed to stop a RUNNING agent.
    fn person() -> (Caller, AgentRecord) {
        (Caller::Person, pm_record())
    }

    fn pm() -> (Caller, AgentRecord) {
        (
            Caller::Agent {
                id: Some(AgentId::new("a-pm").unwrap()),
            },
            pm_record(),
        )
    }

    fn intent(rig: &Rig) -> Intent {
        rig.registry
            .get(&AgentId::new("a-target").unwrap())
            .unwrap()
            .unwrap()
            .intent
    }

    fn kinds(rig: &Rig) -> Vec<String> {
        rig.journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn without_yes_it_prints_the_plan_and_does_nothing() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let ran = go(
            &rig,
            Scenario::default(),
            &c,
            Some(&rec),
            false,
            None,
            ClosedHow::Parked,
            60,
        );
        let Ok(Outcome::DryRun(plan)) = ran.result else {
            panic!("{:?}", ran.result)
        };
        let text = plan.join("\n");
        assert!(
            text.contains("would stop target") && text.contains("pid 100"),
            "{text}"
        );
        assert!(
            text.contains("peer-t"),
            "the plan names the peer it would ask: {text}"
        );
        assert!(text.contains("nothing was done"), "{text}");
        assert!(ran.sent.is_empty(), "nothing was sent");
        assert!(ran.kills.is_empty(), "nothing was killed");
        assert_eq!(intent(&rig), Intent::Wanted);
        assert!(kinds(&rig).is_empty());
    }

    #[test]
    fn the_happy_path_asks_waits_records_the_intent_before_the_kill_then_stops() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let ran = go(
            &rig,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        let Ok(Outcome::Stopped { pid, waited }) = ran.result else {
            panic!("{:?}", ran.result)
        };
        assert_eq!(pid, 100);
        assert!(
            waited >= Duration::from_secs(4),
            "it polled until ready: {waited:?}"
        );
        assert_eq!(ran.sent.len(), 1);
        assert_eq!(ran.sent[0].0, "peer-t");
        assert!(
            ran.sent[0].1.contains("assert-idle") && ran.sent[0].1.contains("HANDOFF"),
            "{}",
            ran.sent[0].1
        );
        assert!(
            ran.sent[0].1.contains("person"),
            "it says who asked: {}",
            ran.sent[0].1
        );
        assert_eq!(
            ran.kills,
            ["kill pid 100 start 5000"],
            "exactly one kill, of exactly that process"
        );
        assert!(
            matches!(intent(&rig), Intent::Closed { how: ClosedHow::Parked, ref by, .. } if by == "person")
        );
        assert_eq!(kinds(&rig), ["down-requested", "closed", "stopped"]);
    }

    #[test]
    fn the_closed_event_is_written_before_the_process_is_touched() {
        // Prove ORDER, not just presence: when the terminator runs, the intent is already Closed.
        struct Peek<'a> {
            rig: &'a Rig,
            seen: RefCell<Option<Intent>>,
        }
        impl Terminator for Peek<'_> {
            fn kill_verified(&self, _: &ProcessIdentity) -> Result<(), KillError> {
                *self.seen.borrow_mut() = Some(intent(self.rig));
                Ok(())
            }
            fn is_gone(&self, _: &ProcessIdentity) -> bool {
                true
            }
        }
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let cfg = Config::default();
        let table = Alive(vec![(100, 5000)]);
        let msg = Msg {
            peers: Ok(vec![Peer {
                id: "peer-t".into(),
                pid: 501,
                cwd: "c".into(),
            }]),
            sent: RefCell::new(vec![]),
            send_fails: false,
        };
        let parents = Parents(HashMap::from([(501, (100, "claude.exe".to_string()))]));
        let tree = Tree(vec![]);
        let facts = Facts_ {
            clock: rig.clock.clone(),
            ready_after_polls: Some(0),
            polls: RefCell::new(0),
        };
        let peek = Peek {
            rig: &rig,
            seen: RefCell::new(None),
        };
        let clock = rig.clock.clone();
        let sleeper = move |d: Duration| clock.advance(d);
        let id = AgentId::new("a-target").unwrap();
        stop_running(
            &Deps {
                registry: &rig.registry,
                journal: &rig.journal,
                cfg: &cfg,
                table: &table,
                messenger: &msg,
                parents: &parents,
                tree: &tree,
                facts: &facts,
                terminator: &peek,
                clock: &*rig.clock,
                sleep: &sleeper,
            },
            &Request {
                target: &id,
                how: ClosedHow::Exited,
                confirm: None,
                yes: true,
                timeout: Duration::from_secs(60),
                poll: Duration::from_secs(1),
                caller: &c,
                caller_rec: Some(&rec),
            },
        )
        .unwrap();
        assert!(
            matches!(
                peek.seen.borrow().as_ref(),
                Some(Intent::Closed {
                    how: ClosedHow::Exited,
                    ..
                })
            ),
            "at the moment of the kill the intent was already recorded: {:?}",
            peek.seen.borrow()
        );
    }

    #[test]
    fn an_agent_that_never_becomes_ready_is_left_running_with_every_blocker_named() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let sc = Scenario {
            ready_after_polls: None,
            shells: vec![(77, "bash".into())],
            ..Scenario::default()
        };
        let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 10);
        let Err(DownError::NotReady { blockers, waited }) = ran.result else {
            panic!("{:?}", ran.result)
        };
        assert!(waited >= Duration::from_secs(10));
        let all = blockers.join(" | ");
        assert!(
            all.contains("no HANDOFF") && all.contains("busy") && all.contains("live shell"),
            "{all}"
        );
        assert!(ran.kills.is_empty(), "nothing was killed");
        assert_eq!(intent(&rig), Intent::Wanted, "the intent was not touched");
        assert_eq!(kinds(&rig), ["down-requested", "down-timeout"]);
    }

    #[test]
    fn a_live_shell_below_the_lane_blocks_even_when_everything_else_is_ready() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let sc = Scenario {
            shells: vec![(77, "pwsh".into())],
            ..Scenario::default()
        };
        let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 8);
        let Err(DownError::NotReady { blockers, .. }) = ran.result else {
            panic!("{:?}", ran.result)
        };
        assert_eq!(blockers.len(), 1, "{blockers:?}");
        assert!(blockers[0].contains("pwsh (pid 77)"));
        assert!(ran.kills.is_empty());
    }

    #[test]
    fn if_the_lane_cannot_be_asked_nothing_is_changed() {
        for (name, sc) in [
            (
                "broker down",
                Scenario {
                    peers: Err("connection refused".into()),
                    ..Scenario::default()
                },
            ),
            (
                "no peer below its claude",
                Scenario {
                    peers: Ok(vec![Peer {
                        id: "p".into(),
                        pid: 999,
                        cwd: "c".into(),
                    }]),
                    ..Scenario::default()
                },
            ),
            (
                "the send fails",
                Scenario {
                    send_fails: true,
                    ..Scenario::default()
                },
            ),
        ] {
            let rig = rig(&[target(), pm_record()]);
            let (c, rec) = person();
            let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 60);
            assert!(
                matches!(ran.result, Err(DownError::CannotAsk(_))),
                "{name}: {:?}",
                ran.result
            );
            assert!(ran.kills.is_empty(), "{name}");
            assert_eq!(intent(&rig), Intent::Wanted, "{name}");
            assert!(!kinds(&rig).contains(&"closed".to_string()), "{name}");
        }
    }

    #[test]
    fn a_recycled_pid_at_kill_time_stops_nothing_and_puts_the_intent_back() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let sc = Scenario {
            kill: Err(KillError::Mismatch {
                expected_start: 5000,
                actual_start: 9999,
            }),
            ..Scenario::default()
        };
        let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 60);
        assert!(
            matches!(ran.result, Err(DownError::IdentityChanged(_))),
            "{:?}",
            ran.result
        );
        assert_eq!(intent(&rig), Intent::Wanted, "the intent was put back");
        let k = kinds(&rig);
        assert_eq!(
            k,
            ["down-requested", "closed", "close-failed"],
            "the attempt is on the record"
        );
    }

    #[test]
    fn a_refused_kill_and_a_process_that_will_not_die_both_put_the_intent_back() {
        for (name, sc) in [
            (
                "refused",
                Scenario {
                    kill: Err(KillError::Failed("access denied".into())),
                    ..Scenario::default()
                },
            ),
            (
                "will not die",
                Scenario {
                    gone_after_checks: 1000,
                    ..Scenario::default()
                },
            ),
        ] {
            let rig = rig(&[target(), pm_record()]);
            let (c, rec) = person();
            let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 60);
            assert!(
                matches!(ran.result, Err(DownError::StopFailed(_))),
                "{name}: {:?}",
                ran.result
            );
            assert_eq!(intent(&rig), Intent::Wanted, "{name}");
            assert!(kinds(&rig).contains(&"close-failed".to_string()), "{name}");
        }
    }

    #[test]
    fn a_process_that_ended_by_itself_in_the_meantime_counts_as_stopped() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let sc = Scenario {
            kill: Err(KillError::NotRunning),
            ..Scenario::default()
        };
        let ran = go(&rig, sc, &c, Some(&rec), true, None, ClosedHow::Parked, 60);
        assert!(
            matches!(ran.result, Ok(Outcome::Stopped { .. })),
            "{:?}",
            ran.result
        );
        assert!(matches!(intent(&rig), Intent::Closed { .. }));
        assert_eq!(kinds(&rig).last().map(String::as_str), Some("stopped"));
    }

    #[test]
    fn policy_a_lane_cannot_stop_itself_or_another_and_a_pinned_agent_needs_its_name() {
        let rig = rig(&[target(), pm_record()]);
        // Itself.
        let me = Caller::Agent {
            id: Some(AgentId::new("a-target").unwrap()),
        };
        let ran = go(
            &rig,
            Scenario::default(),
            &me,
            Some(&target()),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert_eq!(ran.result, Err(DownError::IsSelf));
        // An ordinary other lane.
        let other_rec = {
            let mut r =
                AgentRecord::new(AgentId::new("a-other").unwrap(), "C:/Projects/other", t0());
            r.name = Some("other".into());
            r
        };
        let other = Caller::Agent {
            id: Some(AgentId::new("a-other").unwrap()),
        };
        let ran = go(
            &rig,
            Scenario::default(),
            &other,
            Some(&other_rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(
            matches!(ran.result, Err(DownError::Denied(_))),
            "{:?}",
            ran.result
        );
        // An unregistered / unclear caller.
        let ran = go(
            &rig,
            Scenario::default(),
            &Caller::Agent { id: None },
            None,
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(matches!(ran.result, Err(DownError::Denied(_))));
        let ran = go(
            &rig,
            Scenario::default(),
            &Caller::Unclear("x".into()),
            None,
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(matches!(ran.result, Err(DownError::Denied(_))));
        assert!(ran.kills.is_empty() && ran.sent.is_empty());
        assert_eq!(intent(&rig), Intent::Wanted);
        // A pinned target needs its name typed, for a person too.
        let rig = rig_pinned();
        let ran = go(
            &rig,
            Scenario::default(),
            &Caller::Person,
            None,
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert_eq!(
            ran.result,
            Err(DownError::NeedsConfirmation {
                expected: "target".into()
            })
        );
        let ran = go(
            &rig,
            Scenario::default(),
            &Caller::Person,
            None,
            true,
            Some("TARGET"),
            ClosedHow::Parked,
            60,
        );
        assert!(
            matches!(ran.result, Ok(Outcome::Stopped { .. })),
            "{:?}",
            ran.result
        );
    }

    /// The PM may park a STOPPED lane (control.rs) but not end a RUNNING one: only a person may.
    #[test]
    fn the_pm_is_denied_against_a_running_agent_and_nothing_is_asked_or_killed() {
        let rig = rig(&[target(), pm_record()]);
        let (c, rec) = pm();
        let ran = go(
            &rig,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(
            matches!(&ran.result, Err(DownError::Denied(m)) if m.contains("only a person")),
            "{:?}",
            ran.result
        );
        assert!(
            ran.sent.is_empty() && ran.kills.is_empty(),
            "{:?} {:?}",
            ran.sent,
            ran.kills
        );
        assert_eq!(intent(&rig), Intent::Wanted);
        // Positive control: the same request from a person goes through.
        let (c, rec) = person();
        let ran = go(
            &rig,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(
            matches!(ran.result, Ok(Outcome::Stopped { .. })),
            "{:?}",
            ran.result
        );
    }

    fn rig_pinned() -> Rig {
        let mut t = target();
        t.pinned = Some(crate::registry::Pin {
            by: "person".into(),
            at: t0(),
        });
        rig(&[t])
    }

    #[test]
    fn states_that_are_not_a_running_unclosed_verifiable_agent_are_refused() {
        // Not running.
        let rig1 = rig(&[target(), pm_record()]);
        let (c, rec) = person();
        let ran = go(
            &rig1,
            Scenario {
                alive: vec![],
                ..Scenario::default()
            },
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert_eq!(ran.result, Err(DownError::NotRunning));
        // Already closed.
        let rig2 = rig(&[target(), pm_record()]);
        rig2.registry
            .update(&AgentId::new("a-target").unwrap(), |r| {
                r.intent = Intent::Closed {
                    how: ClosedHow::Parked,
                    by: "person".into(),
                    at: t0(),
                }
            })
            .unwrap();
        let ran = go(
            &rig2,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert_eq!(ran.result, Err(DownError::AlreadyClosed));
        // Alive but recorded without a start time: it cannot be verified.
        let mut unverifiable = target();
        unverifiable.sessions[0].process_start_secs = None;
        let rig3 = rig(&[unverifiable, pm_record()]);
        let ran = go(
            &rig3,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(
            matches!(ran.result, Err(DownError::CannotVerify(_))),
            "{:?}",
            ran.result
        );
        assert!(ran.kills.is_empty() && ran.sent.is_empty());
        // Unknown agent.
        let rig4 = rig(&[pm_record()]);
        let ran = go(
            &rig4,
            Scenario::default(),
            &c,
            Some(&rec),
            true,
            None,
            ClosedHow::Parked,
            60,
        );
        assert!(matches!(ran.result, Err(DownError::NotFound(_))));
    }

    #[test]
    fn the_wrap_up_message_is_built_only_from_values_agentlife_computed() {
        let m = wrap_up_text("synapse", "pm-agent:a-pm", ClosedHow::Exited);
        assert!(m.starts_with("[TASK] topic=wrap-up to=synapse from=agentlife"));
        assert!(m.contains("asked agentlife to stop you"));
        assert!(
            m.contains("do NOT run assert-idle"),
            "the lane is told how to decline"
        );
        assert!(m.contains("a reply goes nowhere"));
    }

    fn real_child() -> std::process::Child {
        #[cfg(windows)]
        let mut c = std::process::Command::new("ping");
        #[cfg(windows)]
        c.args(["-n", "40", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut c = std::process::Command::new("sleep");
        #[cfg(not(windows))]
        c.arg("40");
        c.stdout(std::process::Stdio::null())
            .spawn()
            .expect("start a child")
    }

    /// An identity of the running `pid` that the terminator itself reads back as **alive**.
    ///
    /// Why not just one read: on Linux `sysinfo` has been seen (unproven, issue "start-time identity is
    /// best-effort off Windows") to give a live process a start time one second different from an
    /// earlier read, so `is_gone(&identity_read_a_moment_ago)` answered "gone" for a running child
    /// (CI run 37682812078, attempt 1). The question these tests ask is about the terminator's decisions,
    /// not about that, so each use takes a fresh read and retries until the two reads agree. No tolerance
    /// is added to `identity::check` (PM ruling, 2026-10-07).
    fn identity_that_reads_back(pid: u32) -> ProcessIdentity {
        use crate::identity::{ProcessTable, SysinfoTable};
        for _ in 0..40 {
            if let Some(id) = SysinfoTable.identity_of(pid) {
                if !SysinfoTerminator.is_gone(&id) {
                    return id;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("pid {pid} never gave an identity that reads back as alive");
    }

    /// The only code that ends a process, proven against a REAL one: a wrong start time is refused
    /// and the child is still alive afterwards; the right identity ends exactly that child.
    #[test]
    fn the_real_terminator_refuses_a_wrong_start_time_and_ends_the_right_process() {
        let mut child = real_child();
        let right = identity_that_reads_back(child.id());

        let wrong = ProcessIdentity {
            start_secs: right.start_secs.saturating_sub(1000),
            ..right.clone()
        };
        let refused = SysinfoTerminator.kill_verified(&wrong);
        assert!(
            matches!(refused, Err(KillError::Mismatch { .. })),
            "{refused:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().unwrap().is_none(),
            "a refused kill must leave the process alone"
        );
        // Read again now, not 300 ms ago: see `identity_that_reads_back`.
        let right = identity_that_reads_back(child.id());
        assert!(!SysinfoTerminator.is_gone(&right));

        SysinfoTerminator
            .kill_verified(&right)
            .expect("the right identity is ended");
        let status = child.wait().expect("it was really ended");
        let _ = status;
        assert!(SysinfoTerminator.is_gone(&right));
        // A pid that no longer exists is refused, never a panic or a kill of something else. If the
        // OS has already handed the pid to a new process the answer is a mismatch, which is the
        // same refusal for the reason the start time exists.
        assert!(
            matches!(
                SysinfoTerminator.kill_verified(&right),
                Err(KillError::NotRunning) | Err(KillError::Mismatch { .. })
            ),
            "a gone process must never be killed again"
        );
    }
}
