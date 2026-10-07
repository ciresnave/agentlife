// SPDX-License-Identifier: MIT OR Apache-2.0
//! Executing a plan (M3b): launch in batches, judge each agent by **its own** new session, pace, stop
//! after repeated failures, and write a report. `DESIGN.md` §2.2, §2.3 and §2.5, as revised by
//! `DESIGN-REVISION-1.md` §3 and §6 and `DESIGN-REVISION-2.md` §5.
//!
//! **Nothing calls [`execute`] from a command yet.** `agentlife restore` without `--dry-run` still
//! refuses: starting agents needs a person's consent (M4). The tests drive it with stand-ins they
//! spawned themselves.
//!
//! The rules:
//!
//! * The plan's hash must still match its content; otherwise nothing starts.
//! * A **fatal preflight** failure (the host program does not exist) starts nothing, because it would
//!   fail every agent the same way.
//! * Every agent is **re-checked just before its launch**: if it is running by then it is skipped
//!   (idempotence at execution time, not only at plan time).
//! * Batch 0 (the PM, alone) settles before anything else starts; **a PM that fails to come up does not
//!   stop the others**, and its failure leads the report.
//! * Before each batch, free memory below the floor **holds** every agent not yet started: reported as
//!   held for memory, not as failed.
//! * An agent is **Working** when its own new session is alive and its lane state shows progress past
//!   `SessionStart`; **AwaitingDialog** when it is alive, shows none after `progress_timeout`, and
//!   carries the development-channels flag; **Started** when it is alive at the deadline without that
//!   evidence; **Failed** when its process ended, never appeared, or could not be started.
//! * After `stop_after_failed_batches` batches in a row in which nothing came up, the run **halts** and
//!   starts nothing more.
//! * Only one restore runs at a time: `restore.lock` names its owner by pid and process start time, and a
//!   lock whose owner is gone is reclaimed.

use crate::atomic::write_atomic;
use crate::clock::Clock;
use crate::home::Home;
use crate::identity::{self, Match, ProcessIdentity, ProcessTable};
use crate::journal::Journal;
use crate::launch::{build_tab, Programs, SpawnVia, Spawner};
use crate::plan::{Entry, Plan};
use crate::readiness::read_lane_state;
use crate::registry::Registry;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPORT_SCHEMA: u32 = 1;

/// The clock-source slack found on CI by OverMind: a session whose start time is up to this much
/// before the launch is still the launched one.
const START_SLACK_SECS: i64 = 5;

/// What became of one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "why", rename_all = "snake_case")]
pub enum Outcome {
    Working,
    AwaitingDialog,
    /// Alive at the deadline, with no lane-state evidence either way.
    Started,
    Failed(String),
    /// Not launched because it was already running when its turn came.
    Skipped(String),
    /// Never launched: the run halted, was held for memory, or was refused at the start.
    NotStarted(String),
}

impl Outcome {
    pub fn came_up(&self) -> bool {
        matches!(
            self,
            Outcome::Working | Outcome::AwaitingDialog | Outcome::Started
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryReport {
    pub agent_id: String,
    pub name: Option<String>,
    pub batch: u32,
    pub window: u32,
    pub tab: u32,
    pub outcome: Outcome,
    pub spawned_via: Option<SpawnVia>,
    pub launched_at: Option<DateTime<Utc>>,
    pub seconds_to_settle: Option<u64>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightLine {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// A failure that would fail every agent the same way: nothing is started.
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Summary {
    pub working: u32,
    pub awaiting_dialog: u32,
    pub started: u32,
    pub failed: u32,
    pub skipped: u32,
    pub not_started: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub plan_hash: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    /// Why the run stopped starting agents, if it did (repeated failures).
    pub halted: Option<String>,
    /// Set when free memory held the rest of the run.
    pub held_for_memory: bool,
    pub preflight: Vec<PreflightLine>,
    pub entries: Vec<EntryReport>,
    pub summary: Summary,
}

impl Report {
    pub fn render_text(&self) -> String {
        let s = &self.summary;
        let mut out = format!(
            "restore {}: {} working, {} awaiting dialog, {} started, {} failed, {} skipped, {} not started\n",
            &self.plan_hash[..12.min(self.plan_hash.len())],
            s.working,
            s.awaiting_dialog,
            s.started,
            s.failed,
            s.skipped,
            s.not_started
        );
        if let Some(h) = &self.halted {
            out.push_str(&format!("HALTED: {h}\n"));
        }
        if self.held_for_memory {
            out.push_str("HELD FOR MEMORY: the rest were not started\n");
        }
        for p in self.preflight.iter().filter(|p| !p.ok) {
            out.push_str(&format!(
                "preflight {}: {}{}\n",
                p.name,
                p.detail,
                if p.fatal { " (fatal)" } else { "" }
            ));
        }
        // The PM's result leads, whatever it was.
        let mut entries: Vec<&EntryReport> = self.entries.iter().collect();
        entries.sort_by_key(|e| e.batch);
        for e in entries {
            out.push_str(&format!(
                "  batch {} {} {:?}\n",
                e.batch,
                e.name.as_deref().unwrap_or(&e.agent_id),
                e.outcome
            ));
        }
        out
    }
}

/// What a started agent has done so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSeen {
    pub session_id: String,
    pub alive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Progress {
    pub session: Option<SessionSeen>,
    /// Its lane state shows progress past `SessionStart`.
    pub working: bool,
}

/// Where the launcher learns what happened. The real one reads the registry the hook writes.
pub trait Observer {
    fn is_running(&self, agent_id: &str) -> bool;
    /// The agent's newest session id before the launch (`None` if it has none).
    fn latest_session(&self, agent_id: &str) -> Option<String>;
    /// Progress of the session that began after the launch (not `before`).
    fn progress(
        &self,
        agent_id: &str,
        before: Option<&str>,
        launched_at: DateTime<Utc>,
    ) -> Progress;
}

/// Judges by the registry (written by the hook of the **new** session), the process table, and
/// `lane-restart`'s lane state (read only).
pub struct RegistryObserver<'a> {
    pub registry: &'a Registry,
    pub table: &'a dyn ProcessTable,
    pub lane_state_dir: &'a Path,
}

impl Observer for RegistryObserver<'_> {
    fn is_running(&self, agent_id: &str) -> bool {
        let Some(rec) = crate::registry::AgentId::new(agent_id)
            .ok()
            .and_then(|id| self.registry.get(&id).ok().flatten())
        else {
            return false;
        };
        crate::list::liveness(&rec, self.table).0 != crate::list::Liveness::Stopped
    }

    fn latest_session(&self, agent_id: &str) -> Option<String> {
        let id = crate::registry::AgentId::new(agent_id).ok()?;
        let rec = self.registry.get(&id).ok().flatten()?;
        rec.sessions.last().map(|s| s.session_id.clone())
    }

    fn progress(
        &self,
        agent_id: &str,
        before: Option<&str>,
        launched_at: DateTime<Utc>,
    ) -> Progress {
        let Some(rec) = crate::registry::AgentId::new(agent_id)
            .ok()
            .and_then(|id| self.registry.get(&id).ok().flatten())
        else {
            return Progress::default();
        };
        let earliest = launched_at - ChronoDuration::seconds(START_SLACK_SECS);
        let Some(s) = rec
            .sessions
            .iter()
            .rev()
            .find(|s| Some(s.session_id.as_str()) != before && s.started_at >= earliest)
        else {
            return Progress::default();
        };
        let alive = s.ended_at.is_none()
            && match s.process_start_secs {
                Some(start) => {
                    identity::check(
                        self.table,
                        &ProcessIdentity {
                            pid: s.pid,
                            start_secs: start,
                            exe: None,
                        },
                    ) == Match::Same
                }
                None => self.table.identity_of(s.pid).is_some(),
            };
        let working = read_lane_state(self.lane_state_dir, &s.session_id)
            .and_then(|v| v.updated_by_event)
            .is_some_and(|e| e != "SessionStart");
        Progress {
            session: Some(SessionSeen {
                session_id: s.session_id.clone(),
                alive,
            }),
            working,
        }
    }
}

/// Everything [`execute`] needs from outside; each piece is injected so a test supplies it.
pub struct Deps<'a> {
    pub spawner: &'a dyn Spawner,
    pub observer: &'a dyn Observer,
    pub clock: &'a dyn Clock,
    pub sleep: &'a dyn Fn(Duration),
    /// Free memory in GB now, if it can be read.
    pub memory_gb: &'a dyn Fn() -> Option<f64>,
    pub handoff_exists: &'a dyn Fn(&Entry) -> bool,
    pub journal: &'a Journal,
    pub programs: &'a Programs,
    pub poll: Duration,
    pub progress_timeout: Duration,
    pub stop_after_failed_batches: u32,
    /// The pause between two spawns within a batch (never before the first).
    pub spawn_gap: Duration,
}

/// Does `name` name a program that exists? A path is checked as a path; a bare name is looked up on
/// `PATH` (with `.exe` on Windows). Used only for the preflight: a **spawn** failure is what really
/// decides.
pub fn program_exists(name: &str) -> bool {
    let p = Path::new(name);
    if p.components().count() > 1 || p.is_absolute() {
        return p.is_file();
    }
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd"]
    } else {
        &[""]
    };
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            exts.iter()
                .any(|e| dir.join(format!("{name}{e}")).is_file())
        })
    })
}

/// The checks that would fail every agent the same way, plus warnings. `exists` is injected.
pub fn preflight(programs: &Programs, exists: &dyn Fn(&str) -> bool) -> Vec<PreflightLine> {
    let host_ok = exists(&programs.host);
    let claude_ok = exists(&programs.claude);
    vec![
        PreflightLine {
            name: "host program".into(),
            ok: host_ok,
            detail: if host_ok {
                format!("{} found", programs.host)
            } else {
                format!(
                    "{} was not found; every agent would fail to start",
                    programs.host
                )
            },
            fatal: true,
        },
        PreflightLine {
            name: "claude program".into(),
            ok: claude_ok,
            detail: if claude_ok {
                format!("{} found", programs.claude)
            } else {
                format!(
                    "{} was not found on this PATH (the host may still find it)",
                    programs.claude
                )
            },
            fatal: false,
        },
    ]
}

struct Live {
    idx: usize,
    launched_at: DateTime<Utc>,
    before: Option<String>,
    carries_channel: bool,
}

fn carries_channel(e: &Entry) -> bool {
    e.argv.windows(2).any(|w| {
        w[0] == "--dangerously-load-development-channels" && w[1] == crate::plan::ALLOWED_CHANNEL
    })
}

fn tally(entries: &[EntryReport]) -> Summary {
    let mut s = Summary::default();
    for e in entries {
        match e.outcome {
            Outcome::Working => s.working += 1,
            Outcome::AwaitingDialog => s.awaiting_dialog += 1,
            Outcome::Started => s.started += 1,
            Outcome::Failed(_) => s.failed += 1,
            Outcome::Skipped(_) => s.skipped += 1,
            Outcome::NotStarted(_) => s.not_started += 1,
        }
    }
    s
}

/// Runs the plan. Never panics on an agent's failure: every agent ends in the report.
pub fn execute(plan: &Plan, pre: Vec<PreflightLine>, d: &Deps) -> Report {
    let started_at = d.clock.now();
    let mut reports: Vec<EntryReport> = plan
        .entries
        .iter()
        .map(|e| EntryReport {
            agent_id: e.agent_id.clone(),
            name: e.name.clone(),
            batch: e.batch,
            window: e.window,
            tab: e.tab,
            outcome: Outcome::NotStarted("not reached".into()),
            spawned_via: None,
            launched_at: None,
            seconds_to_settle: None,
            session_id: None,
        })
        .collect();
    let mut halted: Option<String> = None;
    let mut held_for_memory = false;
    let _ = d.journal.append(
        "restore-started",
        None,
        json!({"plan": plan.hash, "entries": plan.entries.len(), "batches": plan.batch_count()}),
    );

    let refuse_all = |reports: &mut Vec<EntryReport>, why: String| {
        for r in reports.iter_mut() {
            r.outcome = Outcome::NotStarted(why.clone());
        }
    };
    if !plan.hash_is_valid() {
        refuse_all(
            &mut reports,
            "the plan's hash does not match its content; nothing was started".into(),
        );
    } else if let Some(p) = pre.iter().find(|p| p.fatal && !p.ok) {
        refuse_all(
            &mut reports,
            format!("preflight failed: {}; nothing was started", p.detail),
        );
    } else {
        run_batches(plan, d, &mut reports, &mut halted, &mut held_for_memory);
    }

    let finished_at = d.clock.now();
    let summary = tally(&reports);
    let _ = d.journal.append(
        "restore-finished",
        None,
        json!({"plan": plan.hash, "summary": summary, "halted": halted, "held_for_memory": held_for_memory}),
    );
    Report {
        schema: REPORT_SCHEMA,
        plan_hash: plan.hash.clone(),
        started_at,
        finished_at,
        halted,
        held_for_memory,
        preflight: pre,
        entries: reports,
        summary,
    }
}

fn run_batches(
    plan: &Plan,
    d: &Deps,
    reports: &mut [EntryReport],
    halted: &mut Option<String>,
    held_for_memory: &mut bool,
) {
    let mut batch_ids: Vec<u32> = plan.entries.iter().map(|e| e.batch).collect();
    batch_ids.dedup();
    let last_batch = batch_ids.last().copied();
    let mut failed_streak = 0u32;

    for b in batch_ids {
        let members: Vec<usize> = plan
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.batch == b)
            .map(|(i, _)| i)
            .collect();

        if let Some(why) = halted.as_deref() {
            for i in members {
                reports[i].outcome = Outcome::NotStarted(format!("halted: {why}"));
            }
            continue;
        }
        if *held_for_memory {
            for i in members {
                reports[i].outcome = Outcome::NotStarted("held for memory".into());
            }
            continue;
        }
        if let Some(g) = (d.memory_gb)() {
            if g < plan.params.free_ram_floor_gb {
                *held_for_memory = true;
                let why = format!(
                    "held for memory: {g:.1} GB free, floor {} GB",
                    plan.params.free_ram_floor_gb
                );
                for i in members {
                    reports[i].outcome = Outcome::NotStarted(why.clone());
                }
                continue;
            }
        }

        // Launch every member of the batch.
        let mut live: Vec<Live> = Vec::new();
        let mut spawned_in_batch = 0u32;
        for &i in &members {
            let e = &plan.entries[i];
            if d.observer.is_running(&e.agent_id) {
                reports[i].outcome = Outcome::Skipped("it is already running".into());
                continue;
            }
            let tab = match build_tab(e, d.programs, (d.handoff_exists)(e)) {
                Ok(t) => t,
                Err(err) => {
                    reports[i].outcome = Outcome::Failed(err.to_string());
                    continue;
                }
            };
            if spawned_in_batch > 0 && !d.spawn_gap.is_zero() {
                (d.sleep)(d.spawn_gap);
            }
            spawned_in_batch += 1;
            let before = d.observer.latest_session(&e.agent_id);
            let launched_at = d.clock.now();
            match d.spawner.spawn(&tab) {
                Ok(via) => {
                    reports[i].spawned_via = Some(via);
                    reports[i].launched_at = Some(launched_at);
                    let _ = d.journal.append(
                        "restore-launched",
                        Some(&e.agent_id),
                        json!({"batch": b, "window": tab.window, "via": via}),
                    );
                    live.push(Live {
                        idx: i,
                        launched_at,
                        before,
                        carries_channel: carries_channel(e),
                    });
                }
                Err(err) => {
                    reports[i].outcome = Outcome::Failed(format!("could not be started: {err}"));
                }
            }
        }

        settle(plan, d, reports, live);

        for &i in &members {
            let _ = d.journal.append(
                "restore-settled",
                Some(&plan.entries[i].agent_id),
                json!({"batch": b, "outcome": reports[i].outcome}),
            );
        }
        let launched = members
            .iter()
            .filter(|&&i| {
                reports[i].launched_at.is_some() || matches!(reports[i].outcome, Outcome::Failed(_))
            })
            .count();
        let came_up = members.iter().any(|&i| reports[i].outcome.came_up());
        if launched > 0 && !came_up {
            failed_streak += 1;
        } else if came_up {
            failed_streak = 0;
        }
        if failed_streak >= d.stop_after_failed_batches {
            let why = format!("{failed_streak} batches in a row had no agent come up");
            let _ = d.journal.append(
                "restore-halted",
                None,
                json!({"why": why, "after_batch": b}),
            );
            *halted = Some(why);
            continue;
        }
        if Some(b) != last_batch {
            (d.sleep)(Duration::from_secs(plan.params.batch_delay_secs));
        }
    }
}

/// Polls until every launched agent has settled or its deadline has passed.
fn settle(plan: &Plan, d: &Deps, reports: &mut [EntryReport], mut live: Vec<Live>) {
    let liveness = Duration::from_secs(plan.params.liveness_timeout_secs);
    while !live.is_empty() {
        let now = d.clock.now();
        let mut still = Vec::new();
        for l in live {
            let e = &plan.entries[l.idx];
            let elapsed = (now - l.launched_at).to_std().unwrap_or_default();
            let p = d
                .observer
                .progress(&e.agent_id, l.before.as_deref(), l.launched_at);
            let seen_alive = p.session.as_ref().is_some_and(|s| s.alive);
            let outcome = match &p.session {
                Some(s) if s.alive && p.working => Some(Outcome::Working),
                Some(s) if !s.alive => {
                    Some(Outcome::Failed("its process ended after it started".into()))
                }
                Some(_) if elapsed >= d.progress_timeout && l.carries_channel => {
                    Some(Outcome::AwaitingDialog)
                }
                _ if elapsed >= liveness => Some(if seen_alive {
                    Outcome::Started
                } else {
                    Outcome::Failed(format!(
                        "no session appeared within {} s",
                        liveness.as_secs()
                    ))
                }),
                _ => None,
            };
            match outcome {
                Some(o) => {
                    let r = &mut reports[l.idx];
                    r.outcome = o;
                    r.seconds_to_settle = Some(elapsed.as_secs());
                    r.session_id = p.session.map(|s| s.session_id);
                }
                None => still.push(l),
            }
        }
        live = still;
        if !live.is_empty() {
            (d.sleep)(d.poll);
        }
    }
}

/// Writes the report to `<home>/reports/restore-<UTC>.json` atomically.
pub fn write_report(home: &Home, report: &Report) -> std::io::Result<PathBuf> {
    let dir = home.reports_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "restore-{}-{}.json",
        report.started_at.format("%Y%m%dT%H%M%S%.3fZ"),
        &report.plan_hash[..12.min(report.plan_hash.len())]
    ));
    let body = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    write_atomic(&path, &body)?;
    Ok(path)
}

/// One restore at a time. The file names its owner by pid and process start time; a second run is
/// told who holds it, and a lock whose owner is gone is reclaimed.
#[derive(Debug)]
pub struct RestoreLock {
    path: PathBuf,
    token: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LockProblem {
    /// Another restore is running.
    Busy {
        pid: u32,
    },
    Io(String),
}

impl std::fmt::Display for LockProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockProblem::Busy { pid } => write!(f, "another restore is running (pid {pid})"),
            LockProblem::Io(e) => write!(f, "cannot take the restore lock: {e}"),
        }
    }
}

impl RestoreLock {
    pub fn acquire(
        home: &Home,
        me: &ProcessIdentity,
        table: &dyn ProcessTable,
    ) -> Result<Self, LockProblem> {
        let path = home.root().join("restore.lock");
        std::fs::create_dir_all(home.root()).map_err(|e| LockProblem::Io(e.to_string()))?;
        let token = format!("{} {}", me.pid, me.start_secs);
        for _ in 0..3 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(token.as_bytes())
                        .map_err(|e| LockProblem::Io(e.to_string()))?;
                    return Ok(Self { path, token });
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let owner = parse_owner(&text);
                    let alive = owner
                        .as_ref()
                        .is_some_and(|o| identity::check(table, o) == Match::Same);
                    if alive {
                        return Err(LockProblem::Busy {
                            pid: owner.map(|o| o.pid).unwrap_or(0),
                        });
                    }
                    // Its owner is gone (or the file is unreadable): reclaim it. Rename first, so
                    // that of several reclaimers only one wins.
                    let grave = path.with_extension(format!("stale.{}", std::process::id()));
                    if std::fs::rename(&path, &grave).is_ok() {
                        let _ = std::fs::remove_file(&grave);
                    }
                }
                Err(e) => return Err(LockProblem::Io(e.to_string())),
            }
        }
        Err(LockProblem::Io("could not reclaim a stale lock".into()))
    }
}

fn parse_owner(text: &str) -> Option<ProcessIdentity> {
    let mut it = text.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let start_secs = it.next()?.parse().ok()?;
    Some(ProcessIdentity {
        pid,
        start_secs,
        exe: None,
    })
}

impl Drop for RestoreLock {
    fn drop(&mut self) {
        // Only the owner's own file: never delete a lock someone else reclaimed and retook.
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::launch::{SpawnError, TabLaunch};
    use crate::plan::{Params, PLAN_SCHEMA};
    use crate::registry::{AgentId, AgentRecord, Session};
    use chrono::TimeZone;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn t0() -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000, 0).unwrap()
    }

    #[derive(Clone, Copy)]
    struct Behavior {
        appears: Option<u64>,
        works: Option<u64>,
        dies: Option<u64>,
    }

    const GOOD: Behavior = Behavior {
        appears: Some(3),
        works: Some(8),
        dies: None,
    };
    const NEVER: Behavior = Behavior {
        appears: None,
        works: None,
        dies: None,
    };

    /// A fleet of pretend agents: what each does after it is launched, by the (manual) clock.
    struct Fleet {
        clock: Arc<ManualClock>,
        behavior: RefCell<HashMap<String, Behavior>>,
        default: Behavior,
        launched: RefCell<HashMap<String, DateTime<Utc>>>,
        order: RefCell<Vec<String>>,
        running_before: HashSet<String>,
        spawn_fails: HashSet<String>,
        spawn_calls: RefCell<Vec<TabLaunch>>,
    }

    impl Fleet {
        fn new(default: Behavior) -> Self {
            Fleet {
                clock: Arc::new(ManualClock::new(t0())),
                behavior: RefCell::new(HashMap::new()),
                default,
                launched: RefCell::new(HashMap::new()),
                order: RefCell::new(Vec::new()),
                running_before: HashSet::new(),
                spawn_fails: HashSet::new(),
                spawn_calls: RefCell::new(Vec::new()),
            }
        }
        fn set(&self, id: &str, b: Behavior) {
            self.behavior.borrow_mut().insert(id.into(), b);
        }
        fn behavior_of(&self, id: &str) -> Behavior {
            self.behavior
                .borrow()
                .get(id)
                .copied()
                .unwrap_or(self.default)
        }
    }

    impl Spawner for Fleet {
        fn spawn(&self, tab: &TabLaunch) -> Result<SpawnVia, SpawnError> {
            self.spawn_calls.borrow_mut().push(tab.clone());
            if self.spawn_fails.contains(&tab.agent_id) {
                return Err(SpawnError("no such program".into()));
            }
            self.launched
                .borrow_mut()
                .insert(tab.agent_id.clone(), self.clock.now());
            self.order.borrow_mut().push(tab.agent_id.clone());
            Ok(SpawnVia::Direct)
        }
    }

    impl Observer for Fleet {
        fn is_running(&self, id: &str) -> bool {
            self.running_before.contains(id)
        }
        fn latest_session(&self, _: &str) -> Option<String> {
            None
        }
        fn progress(&self, id: &str, _: Option<&str>, _: DateTime<Utc>) -> Progress {
            let Some(at) = self.launched.borrow().get(id).copied() else {
                return Progress::default();
            };
            let secs = (self.clock.now() - at).num_seconds().max(0) as u64;
            let b = self.behavior_of(id);
            let Some(a) = b.appears.filter(|a| secs >= *a) else {
                return Progress::default();
            };
            let _ = a;
            let alive = !b.dies.is_some_and(|d| secs >= d);
            Progress {
                session: Some(SessionSeen {
                    session_id: format!("s-{id}"),
                    alive,
                }),
                working: alive && b.works.is_some_and(|w| secs >= w),
            }
        }
    }

    fn entry(i: usize, batch: u32, pm: bool, channel: bool) -> Entry {
        let mut argv = vec!["--name".to_string(), format!("lane{i}")];
        if channel {
            argv.push("--dangerously-load-development-channels".into());
            argv.push("server:claude-peers".into());
        }
        Entry {
            agent_id: format!("a-{i}"),
            name: Some(format!("lane{i}")),
            title: format!("lane{i}"),
            cwd: format!("C:/Projects/lane{i}"),
            argv,
            mode: None,
            pm,
            flags: vec![],
            dropped_args: vec![],
            batch,
            window: i as u32 / 10,
            tab: i as u32 % 10,
        }
    }

    fn plan_of(n: usize, pm: bool, batch_size: u32, channel: bool) -> Plan {
        let entries: Vec<Entry> = (0..n)
            .map(|i| {
                let batch = if pm {
                    if i == 0 {
                        0
                    } else {
                        1 + (i as u32 - 1) / batch_size
                    }
                } else {
                    i as u32 / batch_size
                };
                entry(i, batch, pm && i == 0, channel)
            })
            .collect();
        let mut p = Plan {
            schema: PLAN_SCHEMA,
            params: Params {
                batch_size,
                batch_delay_secs: 45,
                liveness_timeout_secs: 120,
                tabs_per_window: 10,
                max_running: None,
                free_ram_floor_gb: 8.0,
            },
            running_now: 0,
            free_ram_gb: None,
            entries,
            held: vec![],
            excluded: vec![],
            hash: String::new(),
        };
        p.hash = p.compute_hash();
        p
    }

    fn programs() -> Programs {
        Programs {
            host: "lane-restart".into(),
            claude: "claude".into(),
        }
    }

    fn ok_pre() -> Vec<PreflightLine> {
        preflight(&programs(), &|_| true)
    }

    struct Run {
        report: Report,
        fleet: Fleet,
    }

    fn run_with(
        fleet: Fleet,
        plan: &Plan,
        pre: Vec<PreflightLine>,
        memory: &dyn Fn() -> Option<f64>,
        stop_after: u32,
    ) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path(), fleet.clock.clone());
        let clock = fleet.clock.clone();
        let sleep = move |d: Duration| {
            clock.set(clock.now() + ChronoDuration::from_std(d).unwrap());
        };
        let report = {
            let progs = programs();
            let deps = Deps {
                spawner: &fleet,
                observer: &fleet,
                clock: &*fleet.clock,
                sleep: &sleep,
                memory_gb: memory,
                handoff_exists: &|_| true,
                journal: &journal,
                programs: &progs,
                poll: Duration::from_secs(1),
                progress_timeout: Duration::from_secs(20),
                stop_after_failed_batches: stop_after,
                spawn_gap: Duration::ZERO,
            };
            execute(plan, pre, &deps)
        };
        Run { report, fleet }
    }

    fn run(fleet: Fleet, plan: &Plan) -> Run {
        run_with(fleet, plan, ok_pre(), &|| Some(64.0), 2)
    }

    fn outcome<'a>(r: &'a Report, id: &str) -> &'a Outcome {
        &r.entries.iter().find(|e| e.agent_id == id).unwrap().outcome
    }

    #[test]
    fn the_pm_launches_first_and_alone_then_batches_each_after_the_last_settled_and_the_delay() {
        let plan = plan_of(8, true, 3, true); // pm + 7 -> batches 0,1,1,1,2,2,2,3
        let r = run(Fleet::new(GOOD), &plan);
        assert!(r
            .report
            .entries
            .iter()
            .all(|e| e.outcome == Outcome::Working));
        assert_eq!(r.report.summary.working, 8);
        let at = |id: &str| {
            r.report
                .entries
                .iter()
                .find(|e| e.agent_id == id)
                .unwrap()
                .launched_at
                .unwrap()
        };
        // The PM is the earliest and nothing else was launched until it was Working (8 s) + 45 s.
        assert_eq!(r.fleet.order.borrow()[0], "a-0");
        assert!((at("a-1") - at("a-0")).num_seconds() >= 8 + 45);
        // Batch members launch together; the next batch waits for them and the delay.
        assert_eq!(at("a-1"), at("a-3"));
        assert!((at("a-4") - at("a-1")).num_seconds() >= 8 + 45);
        // No batch ever has more than batch_size agents launched-but-unsettled at once.
        let mut peak = 0;
        for probe in &r.report.entries {
            let p = probe.launched_at.unwrap();
            let live = r
                .report
                .entries
                .iter()
                .filter(|e| {
                    let l = e.launched_at.unwrap();
                    l <= p && p < l + ChronoDuration::seconds(e.seconds_to_settle.unwrap() as i64)
                })
                .count();
            peak = peak.max(live);
        }
        assert!(peak <= 3, "peak {peak}");
        // The tab each agent was given is the one the plan assigned.
        let calls = r.fleet.spawn_calls.borrow();
        assert_eq!(calls[0].window, "agentlife-0");
        assert_eq!(calls[0].title, "lane0");
    }

    #[test]
    fn a_pm_that_never_comes_up_does_not_hold_the_others_and_its_failure_is_reported() {
        let plan = plan_of(5, true, 3, true);
        let f = Fleet::new(GOOD);
        f.set("a-0", NEVER);
        let r = run(f, &plan);
        assert_eq!(
            outcome(&r.report, "a-0"),
            &Outcome::Failed("no session appeared within 120 s".into())
        );
        for id in ["a-1", "a-2", "a-3", "a-4"] {
            assert_eq!(outcome(&r.report, id), &Outcome::Working, "{id}");
        }
        assert!(r.report.halted.is_none(), "one failed batch is not a halt");
        assert!(r.report.render_text().contains("Failed"));
    }

    #[test]
    fn repeated_failed_batches_halt_the_run_and_the_rest_are_not_started() {
        let plan = plan_of(20, false, 3, true);
        let r = run(Fleet::new(NEVER), &plan);
        assert_eq!(
            r.fleet.spawn_calls.borrow().len(),
            6,
            "two batches of three, then it stops"
        );
        assert_eq!(r.report.summary.failed, 6);
        assert_eq!(r.report.summary.not_started, 14);
        let why = r.report.halted.as_deref().unwrap();
        assert!(why.contains("2 batches in a row"), "{why}");
        assert!(matches!(
            outcome(&r.report, "a-19"),
            Outcome::NotStarted(w) if w.starts_with("halted")
        ));
    }

    #[test]
    fn a_batch_that_came_up_resets_the_failure_count() {
        // batches: fail, ok, fail, fail -> halts after the 4th, not the 2nd.
        let plan = plan_of(12, false, 3, true);
        let f = Fleet::new(NEVER);
        for id in ["a-3", "a-4", "a-5"] {
            f.set(id, GOOD);
        }
        let r = run(f, &plan);
        assert_eq!(r.fleet.spawn_calls.borrow().len(), 12);
        assert!(
            r.report.halted.is_some(),
            "it halts after the last two failed batches"
        );
        assert_eq!(r.report.summary.working, 3);
        assert_eq!(r.report.summary.failed, 9);
    }

    #[test]
    fn low_memory_before_a_batch_holds_the_rest_as_held_not_failed_and_not_halted() {
        let plan = plan_of(7, false, 3, true);
        let calls = RefCell::new(0);
        let r = run_with(
            Fleet::new(GOOD),
            &plan,
            ok_pre(),
            &|| {
                *calls.borrow_mut() += 1;
                Some(if *calls.borrow() == 1 { 20.0 } else { 5.0 })
            },
            2,
        );
        assert_eq!(r.report.summary.working, 3);
        assert_eq!(r.report.summary.not_started, 4);
        assert!(r.report.held_for_memory);
        assert!(r.report.halted.is_none() && r.report.summary.failed == 0);
        assert!(matches!(
            outcome(&r.report, "a-6"),
            Outcome::NotStarted(w) if w.contains("held for memory")
        ));
        // A reading that cannot be taken holds nothing.
        let r = run_with(Fleet::new(GOOD), &plan, ok_pre(), &|| None, 2);
        assert_eq!(r.report.summary.working, 7);
    }

    #[test]
    fn a_fatal_preflight_failure_starts_nothing_and_a_warning_does_not() {
        let plan = plan_of(3, false, 3, true);
        let pre = preflight(&programs(), &|p| p != "lane-restart");
        let r = run_with(Fleet::new(GOOD), &plan, pre, &|| None, 2);
        assert!(r.fleet.spawn_calls.borrow().is_empty());
        assert!(matches!(
            outcome(&r.report, "a-0"),
            Outcome::NotStarted(w) if w.contains("preflight failed") && w.contains("lane-restart")
        ));
        let pre = preflight(&programs(), &|p| p != "claude");
        assert!(!pre[1].fatal && !pre[1].ok);
        let r = run_with(Fleet::new(GOOD), &plan, pre, &|| None, 2);
        assert_eq!(
            r.report.summary.working, 3,
            "a missing claude on this PATH only warns"
        );
    }

    #[test]
    fn a_plan_whose_content_no_longer_matches_its_hash_starts_nothing() {
        let mut plan = plan_of(3, false, 3, true);
        plan.entries[1].cwd = "C:/Projects/elsewhere".into();
        let r = run(Fleet::new(GOOD), &plan);
        assert!(r.fleet.spawn_calls.borrow().is_empty());
        assert!(matches!(
            outcome(&r.report, "a-1"),
            Outcome::NotStarted(w) if w.contains("hash does not match")
        ));
    }

    #[test]
    fn an_agent_already_running_when_its_turn_comes_is_skipped_not_launched_twice() {
        let plan = plan_of(4, false, 3, true);
        let mut f = Fleet::new(GOOD);
        f.running_before.insert("a-2".into());
        let r = run(f, &plan);
        assert!(matches!(outcome(&r.report, "a-2"), Outcome::Skipped(_)));
        assert!(r
            .fleet
            .spawn_calls
            .borrow()
            .iter()
            .all(|t| t.agent_id != "a-2"));
        assert_eq!(r.report.summary.working, 3);
    }

    #[test]
    fn a_spawn_error_and_an_unsafe_argument_fail_that_agent_only() {
        let mut plan = plan_of(4, false, 4, true);
        plan.entries[2].cwd = "C:/Projects/a;b".into();
        plan.hash = plan.compute_hash();
        let mut f = Fleet::new(GOOD);
        f.spawn_fails.insert("a-1".into());
        let r = run(f, &plan);
        assert!(matches!(
            outcome(&r.report, "a-1"),
            Outcome::Failed(w) if w.contains("could not be started") && w.contains("no such program")
        ));
        assert!(matches!(
            outcome(&r.report, "a-2"),
            Outcome::Failed(w) if w.contains("unsafe for wt.exe")
        ));
        assert!(
            r.fleet
                .spawn_calls
                .borrow()
                .iter()
                .all(|t| t.agent_id != "a-2"),
            "the unsafe one was refused before anything started"
        );
        assert_eq!(outcome(&r.report, "a-0"), &Outcome::Working);
        assert_eq!(outcome(&r.report, "a-3"), &Outcome::Working);
    }

    #[test]
    fn alive_but_silent_is_awaiting_dialog_with_the_channel_flag_and_started_without_it() {
        let silent = Behavior {
            appears: Some(3),
            works: None,
            dies: None,
        };
        let r = run(Fleet::new(silent), &plan_of(1, false, 3, true));
        assert_eq!(outcome(&r.report, "a-0"), &Outcome::AwaitingDialog);
        assert_eq!(r.report.entries[0].seconds_to_settle, Some(20));
        let r = run(Fleet::new(silent), &plan_of(1, false, 3, false));
        assert_eq!(outcome(&r.report, "a-0"), &Outcome::Started);
        assert_eq!(r.report.entries[0].seconds_to_settle, Some(120));
    }

    #[test]
    fn a_process_that_ends_after_starting_is_failed_not_working() {
        let dies = Behavior {
            appears: Some(2),
            works: None,
            dies: Some(10),
        };
        let r = run(Fleet::new(dies), &plan_of(1, false, 3, true));
        assert_eq!(
            outcome(&r.report, "a-0"),
            &Outcome::Failed("its process ended after it started".into())
        );
    }

    #[test]
    fn the_journal_records_the_run_and_the_report_round_trips_through_its_file() {
        let plan = plan_of(2, true, 3, true);
        let dir = tempfile::tempdir().unwrap();
        let fleet = Fleet::new(GOOD);
        let journal = Journal::new(dir.path().join("registry"), fleet.clock.clone());
        let clock = fleet.clock.clone();
        let sleep =
            move |d: Duration| clock.set(clock.now() + ChronoDuration::from_std(d).unwrap());
        let progs = programs();
        let report = execute(
            &plan,
            ok_pre(),
            &Deps {
                spawner: &fleet,
                observer: &fleet,
                clock: &*fleet.clock,
                sleep: &sleep,
                memory_gb: &|| None,
                handoff_exists: &|_| true,
                journal: &journal,
                programs: &progs,
                poll: Duration::from_secs(1),
                progress_timeout: Duration::from_secs(20),
                stop_after_failed_batches: 2,
                spawn_gap: Duration::ZERO,
            },
        );
        let kinds: Vec<String> = journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds.first().unwrap(), "restore-started");
        assert_eq!(kinds.last().unwrap(), "restore-finished");
        assert_eq!(kinds.iter().filter(|k| *k == "restore-launched").count(), 2);
        assert_eq!(kinds.iter().filter(|k| *k == "restore-settled").count(), 2);
        let home = Home::new(dir.path().join("home")).unwrap();
        let path = write_report(&home, &report).unwrap();
        assert!(path.starts_with(home.reports_dir()));
        let back: Report = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn spawns_within_a_batch_are_spaced_by_the_gap_and_the_first_is_never_delayed() {
        let plan = plan_of(4, true, 3, true); // pm, then a batch of three
        let dir = tempfile::tempdir().unwrap();
        let fleet = Fleet::new(GOOD);
        let journal = Journal::new(dir.path(), fleet.clock.clone());
        let clock = fleet.clock.clone();
        let sleep =
            move |d: Duration| clock.set(clock.now() + ChronoDuration::from_std(d).unwrap());
        let progs = programs();
        let report = execute(
            &plan,
            ok_pre(),
            &Deps {
                spawner: &fleet,
                observer: &fleet,
                clock: &*fleet.clock,
                sleep: &sleep,
                memory_gb: &|| None,
                handoff_exists: &|_| true,
                journal: &journal,
                programs: &progs,
                poll: Duration::from_secs(1),
                progress_timeout: Duration::from_secs(20),
                stop_after_failed_batches: 2,
                spawn_gap: Duration::from_secs(2),
            },
        );
        let at = |id: &str| {
            report
                .entries
                .iter()
                .find(|e| e.agent_id == id)
                .unwrap()
                .launched_at
                .unwrap()
        };
        // The PM is a batch of one: no gap before it.
        assert_eq!(at("a-0"), report.started_at);
        // The next batch: the first spawn is not delayed by the gap, the others each wait for it.
        let first = at("a-1");
        assert_eq!((at("a-2") - first).num_seconds(), 2);
        assert_eq!((at("a-3") - first).num_seconds(), 4);
        assert!(report.entries.iter().all(|e| e.outcome == Outcome::Working));
        // Each agent's settle time is counted from its own launch, not from the batch's start.
        assert!(report
            .entries
            .iter()
            .filter(|e| e.batch == 1)
            .all(|e| e.seconds_to_settle == Some(8)));
    }

    fn fleet_sim(n: usize, batch: u32) {
        let plan = plan_of(n, true, batch, true);
        let r = run(Fleet::new(GOOD), &plan);
        assert_eq!(r.report.summary.working as usize, n);
        assert_eq!(r.fleet.order.borrow()[0], "a-0", "the PM is first");
        // Fake time: each batch costs its settle time (8 s) plus the delay, except the last.
        let batches = plan.batch_count() as i64;
        let elapsed = (r.report.finished_at - r.report.started_at).num_seconds();
        assert_eq!(elapsed, batches * 8 + (batches - 1) * 45, "{n} agents");
        assert!(r.report.halted.is_none());
    }

    #[test]
    fn a_fleet_of_40_stand_ins_is_paced_exactly() {
        fleet_sim(40, 3);
    }

    #[test]
    fn a_fleet_of_1000_stand_ins_is_paced_exactly() {
        fleet_sim(1000, 5);
    }

    // ---- the registry observer, over a real registry ----

    fn observer_world() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::new(dir.path().join("agents"));
        (dir, reg)
    }

    struct Alive(HashMap<u32, u64>);
    impl ProcessTable for Alive {
        fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
            self.0.get(&pid).map(|s| ProcessIdentity {
                pid,
                start_secs: *s,
                exe: None,
            })
        }
    }

    fn session(id: &str, pid: u32, start: Option<u64>, at: DateTime<Utc>) -> Session {
        Session {
            session_id: id.into(),
            pid,
            process_start_secs: start,
            started_at: at,
            ended_at: None,
            end_reason: None,
        }
    }

    #[test]
    fn the_observer_sees_the_new_session_by_its_own_id_and_never_the_old_one() {
        let (dir, reg) = observer_world();
        let id = AgentId::new("a-x").unwrap();
        let mut rec = AgentRecord::new(id.clone(), "C:/Projects/x", t0());
        rec.sessions.push(session("old", 10, Some(100), t0()));
        reg.upsert(&id, || rec.clone(), |_| {}).unwrap();
        let table = Alive(HashMap::from([(10, 100)]));
        let lane_state = dir.path().join("lane-state");
        std::fs::create_dir_all(&lane_state).unwrap();
        let obs = RegistryObserver {
            registry: &reg,
            table: &table,
            lane_state_dir: &lane_state,
        };
        let launched = t0() + ChronoDuration::seconds(1000);
        assert_eq!(obs.latest_session("a-x").as_deref(), Some("old"));
        assert!(
            obs.is_running("a-x"),
            "the old session is alive in the table"
        );
        // Nothing new yet: the old session, even though it is alive, is not progress.
        assert_eq!(
            obs.progress("a-x", Some("old"), launched),
            Progress::default()
        );
        // The new session registers (its process is alive, started just after the launch).
        reg.upsert(
            &id,
            || rec.clone(),
            |r| {
                r.sessions.push(session(
                    "new",
                    20,
                    Some(200),
                    launched + ChronoDuration::seconds(3),
                ));
            },
        )
        .unwrap();
        let table = Alive(HashMap::from([(10, 100), (20, 200)]));
        let obs = RegistryObserver {
            registry: &reg,
            table: &table,
            lane_state_dir: &lane_state,
        };
        let p = obs.progress("a-x", Some("old"), launched);
        assert_eq!(
            p.session,
            Some(SessionSeen {
                session_id: "new".into(),
                alive: true
            })
        );
        assert!(!p.working, "no lane state yet");
        // SessionStart in the lane state is not progress; any later event is.
        let state = |event: &str| {
            serde_json::json!({
                "session_id": "new", "updated_at": launched, "updated_by_event": event
            })
            .to_string()
        };
        std::fs::write(lane_state.join("x.json"), state("SessionStart")).unwrap();
        assert!(!obs.progress("a-x", Some("old"), launched).working);
        std::fs::write(lane_state.join("x.json"), state("UserPromptSubmit")).unwrap();
        assert!(obs.progress("a-x", Some("old"), launched).working);
        // The process dies: alive is false (same pid, other start time counts as dead too).
        let gone = Alive(HashMap::from([(10, 100), (20, 999)]));
        let obs = RegistryObserver {
            registry: &reg,
            table: &gone,
            lane_state_dir: &lane_state,
        };
        assert!(
            !obs.progress("a-x", Some("old"), launched)
                .session
                .unwrap()
                .alive
        );
    }

    #[test]
    fn the_session_the_agent_already_had_is_excluded_by_id_even_when_it_began_within_the_slack() {
        // The old session began 2 s before the launch: inside the 5 s slack, so only its id keeps
        // it from being mistaken for the launched one.
        let (dir, reg) = observer_world();
        let id = AgentId::new("a-x").unwrap();
        let launched = t0() + ChronoDuration::seconds(1000);
        let mut rec = AgentRecord::new(id.clone(), "C:/Projects/x", t0());
        rec.sessions.push(session(
            "old",
            40,
            Some(7),
            launched - ChronoDuration::seconds(2),
        ));
        reg.upsert(&id, || rec.clone(), |_| {}).unwrap();
        let table = Alive(HashMap::from([(40, 7)]));
        let obs = RegistryObserver {
            registry: &reg,
            table: &table,
            lane_state_dir: dir.path(),
        };
        assert_eq!(
            obs.progress("a-x", Some("old"), launched),
            Progress::default()
        );
        // Without being told it is the old one, the same session is taken for the new one: the
        // control that shows the exclusion above is the id check and not something else.
        assert_eq!(
            obs.progress("a-x", None, launched)
                .session
                .unwrap()
                .session_id,
            "old"
        );
    }

    #[test]
    fn a_session_that_started_long_before_the_launch_is_not_the_launched_one() {
        let (dir, reg) = observer_world();
        let id = AgentId::new("a-x").unwrap();
        let launched = t0() + ChronoDuration::seconds(1000);
        let mut rec = AgentRecord::new(id.clone(), "C:/Projects/x", t0());
        // A different session id, but it began 100 s BEFORE the launch: not ours.
        rec.sessions.push(session(
            "other",
            30,
            Some(5),
            launched - ChronoDuration::seconds(100),
        ));
        reg.upsert(&id, || rec.clone(), |_| {}).unwrap();
        let table = Alive(HashMap::from([(30, 5)]));
        let obs = RegistryObserver {
            registry: &reg,
            table: &table,
            lane_state_dir: dir.path(),
        };
        assert_eq!(obs.progress("a-x", None, launched), Progress::default());
        // Within the 5 s clock slack it still counts.
        reg.upsert(
            &id,
            || rec.clone(),
            |r| {
                r.sessions.push(session(
                    "near",
                    31,
                    Some(6),
                    launched - ChronoDuration::seconds(4),
                ));
            },
        )
        .unwrap();
        let table = Alive(HashMap::from([(30, 5), (31, 6)]));
        let obs = RegistryObserver {
            registry: &reg,
            table: &table,
            lane_state_dir: dir.path(),
        };
        assert_eq!(
            obs.progress("a-x", None, launched)
                .session
                .unwrap()
                .session_id,
            "near"
        );
        // An unknown agent is simply nothing.
        assert_eq!(
            obs.progress("a-nobody", None, launched),
            Progress::default()
        );
        assert!(!obs.is_running("a-nobody"));
    }

    // ---- the lock ----

    #[test]
    fn only_one_restore_runs_at_a_time_and_a_dead_owners_lock_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path()).unwrap();
        let me = ProcessIdentity {
            pid: 500,
            start_secs: 50,
            exe: None,
        };
        let other = ProcessIdentity {
            pid: 600,
            start_secs: 60,
            exe: None,
        };
        let both_alive = Alive(HashMap::from([(500, 50), (600, 60)]));
        let first = RestoreLock::acquire(&home, &me, &both_alive).unwrap();
        // A second run is told who holds it.
        assert_eq!(
            RestoreLock::acquire(&home, &other, &both_alive).unwrap_err(),
            LockProblem::Busy { pid: 500 }
        );
        // The same pid with a different start time is a recycled pid, not the owner: reclaimed.
        let recycled = Alive(HashMap::from([(500, 9999), (600, 60)]));
        let second = RestoreLock::acquire(&home, &other, &recycled).unwrap();
        // The first holder dropping must not delete the lock now owned by the second.
        drop(first);
        assert!(home.root().join("restore.lock").exists());
        assert_eq!(
            RestoreLock::acquire(&home, &me, &both_alive).unwrap_err(),
            LockProblem::Busy { pid: 600 }
        );
        drop(second);
        assert!(!home.root().join("restore.lock").exists());
        // A garbage lock file is reclaimed, never trusted.
        std::fs::write(home.root().join("restore.lock"), "not an owner").unwrap();
        let again = RestoreLock::acquire(&home, &me, &both_alive).unwrap();
        drop(again);
        assert!(!home.root().join("restore.lock").exists());
    }

    #[test]
    fn program_exists_checks_a_path_as_a_path_and_a_bare_name_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("host.bin");
        std::fs::write(&f, "x").unwrap();
        assert!(program_exists(&f.display().to_string()));
        assert!(!program_exists(
            &dir.path().join("missing.bin").display().to_string()
        ));
        assert!(
            !program_exists(&dir.path().display().to_string()),
            "a directory is not a program"
        );
        assert!(!program_exists("agentlife-no-such-program-anywhere"));
    }
}
