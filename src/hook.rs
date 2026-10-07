// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife hook SessionStart|SessionEnd`: how an agent registers itself (DESIGN-REVISION-1 §2.4,
//! DESIGN-REVISION-2 §9).
//!
//! Two events only, never the per-tool-call hook. The hook is **dumb**: it records facts and never
//! decides intent (that is M2). It **never fails the session**: every outcome, including an error,
//! is a normal exit, and a skip or an error is written to `hook.log`, because a hook that fails is
//! silent to Claude Code and a broken install would otherwise never be seen (the lesson of
//! OverMind's `hook-errors.log`, and of its first install that wrote no state file at all).
//!
//! What it reuses from OverMind's `lane-restart`, as a **temporary copy** in `claude_proc.rs` (the
//! library is not on crates.io and the portfolio forbids a new `git =` dependency; see that file's
//! header): the walk from this hook process up to its `claude` (`claude_parent_pid`, which passes
//! through the Git Bash layers a real hook runs under), the launch-flag parser
//! (`parse_claude_cli_flags`, which reads `-n` / `--name`, `--permission-mode`, `--remote-control`)
//! and the launch-directory rule (`recorded_cwd`).
//!
//! What it records, and what it deliberately does not register:
//! * **Interactive sessions only.** A session whose command line has `-p`/`--print`, or which has
//!   another `claude` above it, is a child or headless run and is *not* an agent to restore;
//!   otherwise a fan-out of subagents would turn into a restore storm (DESIGN-REVISION-1 §2.3).
//! * **Never a delete.** `SessionEnd` stamps `ended_at` and the reason; it removes nothing.

use crate::claude_proc::{
    claude_parent_pid, parse_claude_cli_flags, recorded_cwd, HookInput, ModelField, ParentProcess,
};
use crate::identity::{self, Match, ProcessIdentity, ProcessTable};
use crate::journal::Journal;
use crate::procindex::ProcIndex;
use crate::registry::{AgentId, AgentRecord, Origin, Registry, Session};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// The most sessions kept per agent; older ones fall off the front. The journal keeps the full
/// history, so this bounds the *record* without losing the audit trail.
pub const MAX_SESSIONS_KEPT: usize = 20;

/// The reasons `SessionEnd` documents as matchers (hooks docs, fetched 2026-10-06).
pub const KNOWN_END_REASONS: &[&str] = &["clear", "resume", "logout", "prompt_input_exit", "other"];

const SHELLS: &[&str] = &["bash", "sh", "cmd", "powershell", "pwsh"];
const TERMINALS: &[&str] = &["windowsterminal", "openconsole", "conhost", "wt"];
const HOSTS: &[&str] = &["lane-restart", "agentlife"];

/// Everything the hook needs from the machine, so it can be tested without one.
pub trait HookEnv {
    /// The `claude` process this hook runs under, found by walking up through shell layers.
    fn claude_pid(&self) -> Result<u32, String>;
    fn cmdline(&self, pid: u32) -> Option<Vec<String>>;
    fn start_time(&self, pid: u32) -> Option<u64>;
    /// Image names above `pid`, nearest first, lower-case and without `.exe`.
    fn ancestor_images(&self, pid: u32) -> Vec<String>;
    fn var(&self, name: &str) -> Option<String>;
    fn now(&self) -> DateTime<Utc>;
}

pub struct Ctx<'a> {
    pub registry: &'a Registry,
    pub procs: &'a ProcIndex,
    pub journal: &'a Journal,
    pub env: &'a dyn HookEnv,
    pub table: &'a dyn ProcessTable,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Registered {
        agent_id: AgentId,
        created: bool,
    },
    Ended {
        agent_id: AgentId,
    },
    /// Deliberately not registered; the string says why.
    Skipped(String),
    Error(String),
}

impl Outcome {
    /// The one line `hook.log` gets.
    pub fn log_line(&self, event: &str, at: DateTime<Utc>) -> String {
        let what = match self {
            Outcome::Registered { agent_id, created } => {
                format!(
                    "ok {} {agent_id}",
                    if *created { "registered" } else { "updated" }
                )
            }
            Outcome::Ended { agent_id } => format!("ok ended {agent_id}"),
            Outcome::Skipped(why) => format!("skipped: {why}"),
            Outcome::Error(e) => format!("ERROR: {e}"),
        };
        format!("{} {event} {what}", at.to_rfc3339())
    }
}

fn norm_image(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_string()
}

/// `WindowsTerminal -> shell -> claude` is a hand launch; a `lane-restart`/`agentlife` parent is a
/// host launch. The first non-shell image above `claude` decides.
pub fn classify_origin(ancestors: &[String]) -> Origin {
    match ancestors.iter().find(|a| !SHELLS.contains(&a.as_str())) {
        Some(a) if HOSTS.contains(&a.as_str()) => Origin::Host,
        Some(a) if TERMINALS.contains(&a.as_str()) => Origin::Hand,
        _ => Origin::Other,
    }
}

fn is_headless(cmdline: &[String]) -> bool {
    cmdline.iter().any(|a| a == "-p" || a == "--print")
}

fn model_string(m: &ModelField) -> String {
    match m {
        ModelField::Plain(s) => s.clone(),
        ModelField::WithId { id } => id.clone(),
    }
}

/// Handles one hook invocation. `stdin_json` is the hook payload; `reason` is the `--reason`
/// argument a `SessionEnd` entry passes.
pub fn run(ctx: &Ctx, event: &str, reason: Option<&str>, stdin_json: &str) -> Outcome {
    let input: HookInput = match serde_json::from_str(stdin_json) {
        Ok(i) => i,
        Err(e) => return Outcome::Error(format!("could not parse hook input: {e}")),
    };
    let source = serde_json::from_str::<serde_json::Value>(stdin_json)
        .ok()
        .and_then(|v| v.get("source").and_then(|s| s.as_str()).map(str::to_string));
    let result = match event {
        "SessionStart" => session_start(ctx, &input, source.as_deref()),
        "SessionEnd" => session_end(ctx, &input, reason),
        other => Err(format!("not a hook event this command handles: {other}")),
    };
    result.unwrap_or_else(Outcome::Error)
}

fn identity_alive(ctx: &Ctx, s: &Session) -> bool {
    match s.process_start_secs {
        Some(start) => {
            identity::check(
                ctx.table,
                &ProcessIdentity {
                    pid: s.pid,
                    start_secs: start,
                    exe: None,
                },
            ) == Match::Same
        }
        // Without a start time a pid cannot be verified; treat it as alive so a live agent's
        // record is never taken over. The cost is at worst one duplicate record.
        None => ctx.table.identity_of(s.pid).is_some(),
    }
}

/// Which agent does this process belong to? Strongest evidence first:
/// 1. `AGENTLIFE_AGENT_ID` in the environment (set by a launcher; exact across restarts);
/// 2. a pointer for exactly this process (a continuation: `/clear`, compaction, `--resume`);
/// 3. a record with the same name and launch directory whose last process is gone (a hand-started
///    agent started again after a restart);
/// 4. otherwise a new agent.
fn resolve_agent(
    ctx: &Ctx,
    claude: u32,
    start: Option<u64>,
    name: &Option<String>,
    cwd: &str,
) -> Result<AgentId, String> {
    if let Some(v) = ctx.env.var("AGENTLIFE_AGENT_ID") {
        if !v.is_empty() {
            match AgentId::new(v.clone()) {
                Ok(id) => return Ok(id),
                Err(e) => {
                    // Fall through to inference; the caller logs nothing here, but the journal
                    // entry records which rule was used.
                    let _ = e;
                }
            }
        }
    }
    if let Some(start) = start {
        if let Some(id) = ctx.procs.get(claude, start) {
            return Ok(id);
        }
    }
    let listing = ctx.registry.list().map_err(|e| e.to_string())?;
    let mut candidates: Vec<&AgentRecord> = listing
        .records
        .iter()
        .filter(|r| {
            let same_name = match (&r.name, name) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                (None, None) => true,
                _ => false,
            };
            same_name
                && identity::paths_equal(&r.launch_cwd, cwd)
                && r.sessions.last().is_none_or(|s| !identity_alive(ctx, s))
        })
        .collect();
    candidates.sort_by_key(|r| r.first_seen);
    Ok(match candidates.first() {
        Some(r) => r.agent_id.clone(),
        None => AgentId::generate(),
    })
}

struct StartFacts<'a> {
    session_id: &'a str,
    pid: u32,
    start: Option<u64>,
    now: DateTime<Utc>,
    source: Option<&'a str>,
}

/// Applies a `SessionStart` to a record. Pure, so every branch is unit tested.
fn apply_session_start(rec: &mut AgentRecord, f: &StartFacts) {
    // The same process moving to a new session (`/clear`, `/resume`, a fork): the old session
    // ended, and the reason is the documented `source`.
    for s in rec.sessions.iter_mut() {
        if s.pid == f.pid
            && s.process_start_secs == f.start
            && s.ended_at.is_none()
            && s.session_id != f.session_id
        {
            s.ended_at = Some(f.now);
            s.end_reason = Some(match f.source {
                Some(x @ ("clear" | "resume" | "fork")) => x.to_string(),
                _ => "replaced".to_string(),
            });
        }
    }
    match rec
        .sessions
        .iter_mut()
        .find(|s| s.session_id == f.session_id)
    {
        // Compaction fires `SessionStart` again for the same session: not a new launch.
        Some(s) => {
            s.pid = f.pid;
            s.process_start_secs = f.start;
        }
        None => rec.sessions.push(Session {
            session_id: f.session_id.to_string(),
            pid: f.pid,
            process_start_secs: f.start,
            started_at: f.now,
            ended_at: None,
            end_reason: None,
        }),
    }
    if rec.sessions.len() > MAX_SESSIONS_KEPT {
        let drop = rec.sessions.len() - MAX_SESSIONS_KEPT;
        rec.sessions.drain(..drop);
    }
}

fn session_start(ctx: &Ctx, input: &HookInput, source: Option<&str>) -> Result<Outcome, String> {
    let claude = match ctx.env.claude_pid() {
        Ok(p) => p,
        Err(e) => return Ok(Outcome::Skipped(format!("not under a claude process: {e}"))),
    };
    let cmdline = ctx.env.cmdline(claude).unwrap_or_default();
    if is_headless(&cmdline) {
        return Ok(Outcome::Skipped("headless session (-p/--print)".into()));
    }
    let ancestors = ctx.env.ancestor_images(claude);
    if ancestors.iter().any(|a| a == "claude") {
        return Ok(Outcome::Skipped(
            "child session: another claude is above this one".into(),
        ));
    }
    let flags = parse_claude_cli_flags(&cmdline);
    let start = ctx.env.start_time(claude);
    let now = ctx.env.now();
    let origin = classify_origin(&ancestors);
    let model = input.model.as_ref().map(model_string);
    let launch_cwd = recorded_cwd(None, input, claude);
    let id = resolve_agent(ctx, claude, start, &flags.name, &launch_cwd)?;
    let role = ctx.env.var("LANE_ROLE").filter(|r| !r.is_empty());

    let facts = StartFacts {
        session_id: &input.session_id,
        pid: claude,
        start,
        now,
        source,
    };
    let (rec, created) = ctx
        .registry
        .upsert(
            &id,
            || AgentRecord::new(id.clone(), launch_cwd.clone(), now),
            |r| {
                if flags.name.is_some() {
                    r.name = flags.name.clone();
                }
                if role.is_some() {
                    r.role = role.clone();
                }
                // Always the argv of THIS launch, never carried forward (OverMind's rule: a stale
                // argv is as wrong as an invented one).
                r.launch_args = if cmdline.is_empty() {
                    None
                } else {
                    Some(cmdline.clone())
                };
                if model.is_some() {
                    r.model = model.clone();
                }
                if let Some(m) = input
                    .permission_mode
                    .clone()
                    .or_else(|| flags.permission_mode.clone())
                {
                    r.permission_mode = Some(m);
                }
                r.remote_control = flags.remote_control;
                r.origin = origin;
                apply_session_start(r, &facts);
            },
        )
        .map_err(|e| e.to_string())?;
    if let Some(start) = start {
        ctx.procs
            .put(claude, start, &rec.agent_id)
            .map_err(|e| format!("could not write the process pointer: {e}"))?;
    }
    let _ = ctx.journal.append(
        if created {
            "registered"
        } else {
            "session-started"
        },
        Some(rec.agent_id.as_str()),
        json!({
            "session_id": input.session_id,
            "pid": claude,
            "source": source,
            "origin": origin,
            "name": rec.name,
            "cwd": rec.launch_cwd,
        }),
    );
    Ok(Outcome::Registered {
        agent_id: rec.agent_id,
        created,
    })
}

fn session_end(ctx: &Ctx, input: &HookInput, reason: Option<&str>) -> Result<Outcome, String> {
    let claude = match ctx.env.claude_pid() {
        Ok(p) => p,
        Err(e) => return Ok(Outcome::Skipped(format!("not under a claude process: {e}"))),
    };
    let start = ctx.env.start_time(claude);
    let reason = reason
        .filter(|r| KNOWN_END_REASONS.contains(r))
        .unwrap_or("other");
    // O(1) through the pointer when the start time is known; a scan only without one.
    let id = match start.and_then(|s| ctx.procs.get(claude, s)) {
        Some(id) => Some(id),
        None => ctx
            .registry
            .list()
            .map_err(|e| e.to_string())?
            .records
            .iter()
            .find(|r| {
                r.sessions.iter().any(|s| {
                    s.pid == claude && s.process_start_secs == start && s.ended_at.is_none()
                })
            })
            .map(|r| r.agent_id.clone()),
    };
    let Some(id) = id else {
        return Ok(Outcome::Skipped(
            "no registered session for this process".into(),
        ));
    };
    let now = ctx.env.now();
    let mut stamped = false;
    ctx.registry
        .update(&id, |r| {
            // Prefer the named session; else any still-open session of this process.
            let by_id = r
                .sessions
                .iter()
                .position(|s| s.session_id == input.session_id && s.ended_at.is_none());
            let by_proc = || {
                r.sessions.iter().position(|s| {
                    s.pid == claude && s.process_start_secs == start && s.ended_at.is_none()
                })
            };
            if let Some(i) = by_id.or_else(by_proc) {
                r.sessions[i].ended_at = Some(now);
                r.sessions[i].end_reason = Some(reason.to_string());
                stamped = true;
            }
        })
        .map_err(|e| e.to_string())?;
    if !stamped {
        return Ok(Outcome::Skipped(
            "the session was already ended or is unknown".into(),
        ));
    }
    let _ = ctx.journal.append(
        "session-ended",
        Some(id.as_str()),
        json!({"session_id": input.session_id, "pid": claude, "reason": reason}),
    );
    Ok(Outcome::Ended { agent_id: id })
}

/// One refresh of the process table, served as the copied `ParentProcess` trait.
///
/// **Why not OverMind's own `RealParentProcess` (deliberately not copied)?** It refreshes every process with
/// sysinfo's default (expensive) kind on *every hop*. Measured on this machine (640 processes,
/// 15 live lanes), `parent_of` cost about 860 ms per hop, so a `SessionEnd` walk took 3 to 5.5 s
/// under load against its documented **1.5 s** budget; the real-chain test caught it. One
/// `refresh(All, nothing)` costs about 80 ms. The walk itself is the copied `claude_parent_pid`;
/// only the data source differs, so the shell-skipping and refusal rules are unchanged.
pub struct SnapshotParents {
    /// pid -> (parent pid, image name, start time in seconds)
    procs: HashMap<u32, (Option<u32>, String, u64)>,
}

impl SnapshotParents {
    pub fn capture() -> Self {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        Self {
            procs: sys
                .processes()
                .iter()
                .map(|(pid, p)| {
                    (
                        pid.as_u32(),
                        (
                            p.parent().map(|pp| pp.as_u32()),
                            p.name().to_string_lossy().into_owned(),
                            p.start_time(),
                        ),
                    )
                })
                .collect(),
        }
    }

    #[cfg(test)]
    fn from_entries(entries: &[(u32, Option<u32>, &str, u64)]) -> Self {
        Self {
            procs: entries
                .iter()
                .map(|(pid, ppid, name, start)| (*pid, (*ppid, name.to_string(), *start)))
                .collect(),
        }
    }
}

impl ParentProcess for SnapshotParents {
    fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
        let ppid = self.procs.get(&pid)?.0?;
        let name = self.procs.get(&ppid)?.1.clone();
        Some((ppid, name))
    }

    fn start_time_of(&self, pid: u32) -> Option<u64> {
        self.procs.get(&pid).map(|p| p.2)
    }

    /// The full argv of one process. Read fresh for that single pid with `cmd` set explicitly
    /// (never sysinfo's default, which leaves it empty: OverMind's `cwd`/`cmd` bug), and only for
    /// the `claude` pid, so it never costs a whole-table refresh.
    fn cmdline_of(&self, pid: u32) -> Option<Vec<String>> {
        let p = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[p]),
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        Some(
            sys.process(p)?
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect(),
        )
    }
}

/// The real machine.
pub struct RealEnv {
    parents: SnapshotParents,
}

impl RealEnv {
    pub fn new() -> Self {
        Self {
            parents: SnapshotParents::capture(),
        }
    }
}

impl Default for RealEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl HookEnv for RealEnv {
    fn claude_pid(&self) -> Result<u32, String> {
        claude_parent_pid(std::process::id(), &self.parents).map_err(|e| e.to_string())
    }

    fn cmdline(&self, pid: u32) -> Option<Vec<String>> {
        self.parents.cmdline_of(pid)
    }

    fn start_time(&self, pid: u32) -> Option<u64> {
        self.parents.start_time_of(pid)
    }

    fn ancestor_images(&self, pid: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = pid;
        for _ in 0..16 {
            match self.parents.parent_of(cur) {
                Some((ppid, name)) => {
                    out.push(norm_image(&name));
                    cur = ppid;
                }
                None => break,
            }
        }
        out
    }

    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::registry::{Intent, Registry};
    use chrono::TimeZone;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn t(min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 1, min, 0).unwrap()
    }

    struct FakeEnv {
        claude: Result<u32, String>,
        cmdline: Vec<String>,
        start: Option<u64>,
        ancestors: Vec<String>,
        vars: HashMap<String, String>,
        now: DateTime<Utc>,
    }

    impl FakeEnv {
        fn lane(pid: u32) -> Self {
            Self {
                claude: Ok(pid),
                cmdline: ["claude.exe", "-n", "synapse", "--model", "sonnet"]
                    .map(String::from)
                    .to_vec(),
                start: Some(5000 + pid as u64),
                ancestors: ["pwsh", "windowsterminal", "svchost"]
                    .map(String::from)
                    .to_vec(),
                vars: HashMap::new(),
                now: t(0),
            }
        }
    }

    impl HookEnv for FakeEnv {
        fn claude_pid(&self) -> Result<u32, String> {
            self.claude.clone()
        }
        fn cmdline(&self, _: u32) -> Option<Vec<String>> {
            Some(self.cmdline.clone())
        }
        fn start_time(&self, _: u32) -> Option<u64> {
            self.start
        }
        fn ancestor_images(&self, _: u32) -> Vec<String> {
            self.ancestors.clone()
        }
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn now(&self) -> DateTime<Utc> {
            self.now
        }
    }

    /// A process table that says which `(pid, start)` pairs are alive.
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

    struct Rig {
        _d: tempfile::TempDir,
        registry: Registry,
        procs: ProcIndex,
        journal: Journal,
    }

    fn rig() -> Rig {
        let d = tempfile::tempdir().unwrap();
        Rig {
            registry: Registry::new(d.path().join("agents")),
            procs: ProcIndex::new(d.path().join("procs")),
            journal: Journal::new(d.path().join("journal"), Arc::new(SystemClock)),
            _d: d,
        }
    }

    fn payload(session: &str, source: Option<&str>) -> String {
        let mut v = json!({
            "hook_event_name": "SessionStart",
            "session_id": session,
            "cwd": "C:\\Projects\\synapse",
        });
        if let Some(s) = source {
            v["source"] = json!(s);
        }
        v.to_string()
    }

    fn go(
        r: &Rig,
        env: &FakeEnv,
        alive: &Alive,
        event: &str,
        reason: Option<&str>,
        json: &str,
    ) -> Outcome {
        run(
            &Ctx {
                registry: &r.registry,
                procs: &r.procs,
                journal: &r.journal,
                env,
                table: alive,
            },
            event,
            reason,
            json,
        )
    }

    #[test]
    fn a_session_start_registers_a_new_agent_with_everything_the_command_line_says() {
        let r = rig();
        let mut env = FakeEnv::lane(100);
        env.cmdline = [
            "claude.exe",
            "-n",
            "synapse",
            "--model",
            "sonnet",
            "--permission-mode",
            "auto",
            "--remote-control",
            "--dangerously-load-development-channels",
            "server:claude-peers",
        ]
        .map(String::from)
        .to_vec();
        let out = go(
            &r,
            &env,
            &Alive(vec![(100, 5100)]),
            "SessionStart",
            None,
            &payload("s1", Some("startup")),
        );
        let Outcome::Registered { agent_id, created } = out else {
            panic!("{out:?}")
        };
        assert!(created);
        let rec = r.registry.get(&agent_id).unwrap().unwrap();
        assert_eq!(rec.name.as_deref(), Some("synapse"));
        assert_eq!(rec.permission_mode.as_deref(), Some("auto"));
        assert!(rec.remote_control);
        assert_eq!(rec.origin, Origin::Hand);
        assert_eq!(rec.intent, Intent::Wanted);
        assert_eq!(rec.launch_cwd, "C:\\Projects\\synapse");
        assert_eq!(
            rec.launch_args.as_ref().unwrap(),
            &env.cmdline,
            "the argv verbatim"
        );
        assert_eq!(rec.sessions.len(), 1);
        assert_eq!(rec.sessions[0].pid, 100);
        assert_eq!(rec.sessions[0].process_start_secs, Some(5100));
        assert_eq!(r.procs.get(100, 5100), Some(agent_id.clone()));
        let kinds: Vec<_> = r
            .journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["registered"]);
    }

    #[test]
    fn a_headless_session_is_not_registered() {
        let r = rig();
        let mut env = FakeEnv::lane(100);
        env.cmdline = ["claude.exe", "-p", "do a thing"]
            .map(String::from)
            .to_vec();
        let out = go(
            &r,
            &env,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        );
        assert!(
            matches!(out, Outcome::Skipped(ref w) if w.contains("headless")),
            "{out:?}"
        );
        assert!(r.registry.list().unwrap().records.is_empty());
        // Positive control: the same lane without -p registers.
        env.cmdline = ["claude.exe", "-n", "synapse"].map(String::from).to_vec();
        assert!(matches!(
            go(
                &r,
                &env,
                &Alive(vec![]),
                "SessionStart",
                None,
                &payload("s1", None)
            ),
            Outcome::Registered { .. }
        ));
    }

    #[test]
    fn a_child_session_is_not_registered() {
        let r = rig();
        let mut env = FakeEnv::lane(100);
        env.ancestors = ["pwsh", "claude", "windowsterminal"]
            .map(String::from)
            .to_vec();
        let out = go(
            &r,
            &env,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        );
        assert!(
            matches!(out, Outcome::Skipped(ref w) if w.contains("child session")),
            "{out:?}"
        );
        assert!(r.registry.list().unwrap().records.is_empty());
    }

    #[test]
    fn a_hook_not_under_claude_is_skipped_not_an_error() {
        let r = rig();
        let mut env = FakeEnv::lane(100);
        env.claude = Err("ancestor process is \"node\"".into());
        let out = go(
            &r,
            &env,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        );
        assert!(
            matches!(out, Outcome::Skipped(ref w) if w.contains("not under a claude")),
            "{out:?}"
        );
    }

    #[test]
    fn origin_is_classified_from_the_first_non_shell_ancestor() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_origin(&v(&["pwsh", "windowsterminal", "svchost"])),
            Origin::Hand
        );
        assert_eq!(
            classify_origin(&v(&["lane-restart", "windowsterminal"])),
            Origin::Host
        );
        assert_eq!(
            classify_origin(&v(&["agentlife", "windowsterminal"])),
            Origin::Host
        );
        assert_eq!(
            classify_origin(&v(&["bash", "bash", "node"])),
            Origin::Other
        );
        assert_eq!(classify_origin(&v(&[])), Origin::Other);
        assert_eq!(norm_image("C:\\x\\WindowsTerminal.exe"), "windowsterminal");
    }

    #[test]
    fn a_new_process_of_an_agent_that_died_continues_the_same_record_by_name_and_cwd() {
        let r = rig();
        let first = FakeEnv::lane(100);
        let Outcome::Registered { agent_id: a1, .. } = go(
            &r,
            &first,
            &Alive(vec![(100, 5100)]),
            "SessionStart",
            None,
            &payload("s1", None),
        ) else {
            panic!()
        };
        // The machine restarted: pid 100 is gone, the agent is started by hand again as pid 200.
        let second = FakeEnv::lane(200);
        let out = go(
            &r,
            &second,
            &Alive(vec![(200, 5200)]),
            "SessionStart",
            None,
            &payload("s2", None),
        );
        let Outcome::Registered {
            agent_id: a2,
            created,
        } = out
        else {
            panic!("{out:?}")
        };
        assert_eq!(
            a1, a2,
            "same name and launch directory, previous process gone"
        );
        assert!(!created);
        let rec = r.registry.get(&a1).unwrap().unwrap();
        assert_eq!(rec.sessions.len(), 2);
        assert!(
            rec.sessions[0].ended_at.is_none(),
            "killed, not ended: that is the evidence M2 reads"
        );
    }

    #[test]
    fn a_second_live_lane_with_the_same_name_and_cwd_does_not_take_over_the_first() {
        let r = rig();
        let a = FakeEnv::lane(100);
        let Outcome::Registered { agent_id: a1, .. } = go(
            &r,
            &a,
            &Alive(vec![(100, 5100), (200, 5200)]),
            "SessionStart",
            None,
            &payload("s1", None),
        ) else {
            panic!()
        };
        let b = FakeEnv::lane(200);
        let Outcome::Registered {
            agent_id: a2,
            created,
        } = go(
            &r,
            &b,
            &Alive(vec![(100, 5100), (200, 5200)]),
            "SessionStart",
            None,
            &payload("s2", None),
        )
        else {
            panic!()
        };
        assert_ne!(
            a1, a2,
            "the first lane is alive, so its record is not available"
        );
        assert!(created);
    }

    #[test]
    fn a_different_name_is_a_different_agent() {
        let r = rig();
        let a = FakeEnv::lane(100);
        let Outcome::Registered { agent_id: a1, .. } = go(
            &r,
            &a,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        ) else {
            panic!()
        };
        let mut b = FakeEnv::lane(200);
        b.cmdline = ["claude.exe", "-n", "overmind"].map(String::from).to_vec();
        let Outcome::Registered { agent_id: a2, .. } = go(
            &r,
            &b,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s2", None),
        ) else {
            panic!()
        };
        assert_ne!(a1, a2);
    }

    #[test]
    fn the_agent_id_environment_variable_carries_identity_across_a_restart() {
        let r = rig();
        let mut a = FakeEnv::lane(100);
        a.vars.insert("AGENTLIFE_AGENT_ID".into(), "agent-x".into());
        // A different name each time: only the variable can join them.
        a.cmdline = ["claude.exe", "-n", "first-name"]
            .map(String::from)
            .to_vec();
        go(
            &r,
            &a,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        );
        let mut b = FakeEnv::lane(200);
        b.vars.insert("AGENTLIFE_AGENT_ID".into(), "agent-x".into());
        b.cmdline = ["claude.exe", "-n", "second-name"]
            .map(String::from)
            .to_vec();
        go(
            &r,
            &b,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s2", None),
        );
        let recs = r.registry.list().unwrap().records;
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].agent_id.as_str(), "agent-x");
        assert_eq!(recs[0].sessions.len(), 2);
    }

    #[test]
    fn an_invalid_agent_id_variable_is_ignored_in_favour_of_inference() {
        let r = rig();
        let mut a = FakeEnv::lane(100);
        a.vars
            .insert("AGENTLIFE_AGENT_ID".into(), "bad id; calc".into());
        let Outcome::Registered { agent_id, .. } = go(
            &r,
            &a,
            &Alive(vec![]),
            "SessionStart",
            None,
            &payload("s1", None),
        ) else {
            panic!()
        };
        assert!(
            agent_id.as_str().starts_with("a-"),
            "generated, not the bad value: {agent_id}"
        );
    }

    #[test]
    fn clear_and_compact_in_the_same_process_are_one_agent_with_the_right_sessions() {
        let r = rig();
        let env = FakeEnv::lane(100);
        let alive = Alive(vec![(100, 5100)]);
        go(
            &r,
            &env,
            &alive,
            "SessionStart",
            None,
            &payload("s1", Some("startup")),
        );
        // Compaction: the same session id fires SessionStart again. Not a new launch.
        go(
            &r,
            &env,
            &alive,
            "SessionStart",
            None,
            &payload("s1", Some("compact")),
        );
        let id = r.registry.list().unwrap().records[0].agent_id.clone();
        assert_eq!(r.registry.get(&id).unwrap().unwrap().sessions.len(), 1);
        // /clear: a new session id in the same process. The old one ended with reason "clear".
        let out = go(
            &r,
            &env,
            &alive,
            "SessionStart",
            None,
            &payload("s2", Some("clear")),
        );
        assert!(
            matches!(out, Outcome::Registered { created: false, .. }),
            "{out:?}"
        );
        let rec = r.registry.get(&id).unwrap().unwrap();
        assert_eq!(rec.sessions.len(), 2);
        assert_eq!(rec.sessions[0].end_reason.as_deref(), Some("clear"));
        assert!(rec.sessions[0].ended_at.is_some());
        assert!(rec.sessions[1].ended_at.is_none());
        assert_eq!(r.registry.list().unwrap().records.len(), 1);
    }

    #[test]
    fn session_end_stamps_the_time_and_reason_and_deletes_nothing() {
        let r = rig();
        let mut env = FakeEnv::lane(100);
        let alive = Alive(vec![(100, 5100)]);
        go(
            &r,
            &env,
            &alive,
            "SessionStart",
            None,
            &payload("s1", Some("startup")),
        );
        env.now = t(7);
        let end = payload("s1", None).replace("SessionStart", "SessionEnd");
        let out = go(
            &r,
            &env,
            &alive,
            "SessionEnd",
            Some("prompt_input_exit"),
            &end,
        );
        let Outcome::Ended { agent_id } = out else {
            panic!("{out:?}")
        };
        let rec = r.registry.get(&agent_id).unwrap().unwrap();
        assert_eq!(rec.sessions[0].ended_at, Some(t(7)));
        assert_eq!(
            rec.sessions[0].end_reason.as_deref(),
            Some("prompt_input_exit")
        );
        assert_eq!(
            rec.intent,
            Intent::Wanted,
            "the hook records facts; it never decides intent"
        );
        let kinds: Vec<_> = r
            .journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["registered", "session-ended"]);
        // A second end for the same session changes nothing.
        let again = go(&r, &env, &alive, "SessionEnd", Some("logout"), &end);
        assert!(matches!(again, Outcome::Skipped(_)), "{again:?}");
        assert_eq!(
            r.registry.get(&agent_id).unwrap().unwrap().sessions[0]
                .end_reason
                .as_deref(),
            Some("prompt_input_exit")
        );
    }

    #[test]
    fn an_unknown_end_reason_is_recorded_as_other() {
        let r = rig();
        let env = FakeEnv::lane(100);
        let alive = Alive(vec![(100, 5100)]);
        go(&r, &env, &alive, "SessionStart", None, &payload("s1", None));
        let end = payload("s1", None).replace("SessionStart", "SessionEnd");
        let Outcome::Ended { agent_id } = go(&r, &env, &alive, "SessionEnd", Some("made-up"), &end)
        else {
            panic!()
        };
        assert_eq!(
            r.registry.get(&agent_id).unwrap().unwrap().sessions[0]
                .end_reason
                .as_deref(),
            Some("other")
        );
    }

    #[test]
    fn session_end_for_a_process_that_never_registered_is_skipped() {
        let r = rig();
        let env = FakeEnv::lane(100);
        let end = payload("s1", None).replace("SessionStart", "SessionEnd");
        let out = go(&r, &env, &Alive(vec![]), "SessionEnd", Some("other"), &end);
        assert!(
            matches!(out, Outcome::Skipped(ref w) if w.contains("no registered session")),
            "{out:?}"
        );
    }

    #[test]
    fn session_end_uses_the_pointer_not_a_scan_when_the_start_time_is_known() {
        let r = rig();
        let env = FakeEnv::lane(100);
        let alive = Alive(vec![(100, 5100)]);
        let Outcome::Registered { agent_id, .. } =
            go(&r, &env, &alive, "SessionStart", None, &payload("s1", None))
        else {
            panic!()
        };
        // Make a scan impossible: with the registry directory unreadable as a listing, only the
        // pointer can find the agent. Removing every OTHER file proves nothing, so instead plant a
        // second record that a scan would match first and check the pointer's answer wins.
        let mut decoy = AgentRecord::new(AgentId::new("a-decoy").unwrap(), "C:/x", t(0));
        decoy.sessions.push(Session {
            session_id: "s1".into(),
            pid: 100,
            process_start_secs: Some(5100),
            started_at: t(0),
            ended_at: None,
            end_reason: None,
        });
        r.registry.create(&decoy).unwrap();
        let end = payload("s1", None).replace("SessionStart", "SessionEnd");
        let Outcome::Ended { agent_id: ended } =
            go(&r, &env, &alive, "SessionEnd", Some("other"), &end)
        else {
            panic!()
        };
        assert_eq!(ended, agent_id, "the pointer, not the first scan hit");
        assert!(r
            .registry
            .get(&AgentId::new("a-decoy").unwrap())
            .unwrap()
            .unwrap()
            .sessions[0]
            .ended_at
            .is_none());
    }

    #[test]
    fn sessions_are_capped_but_the_journal_keeps_the_history() {
        let mut rec = AgentRecord::new(AgentId::new("a1").unwrap(), "C:/x", t(0));
        for i in 0..(MAX_SESSIONS_KEPT + 5) {
            apply_session_start(
                &mut rec,
                &StartFacts {
                    session_id: &format!("s{i}"),
                    pid: 1,
                    start: Some(1),
                    now: t(0),
                    source: Some("clear"),
                },
            );
        }
        assert_eq!(rec.sessions.len(), MAX_SESSIONS_KEPT);
        assert_eq!(
            rec.sessions.last().unwrap().session_id,
            format!("s{}", MAX_SESSIONS_KEPT + 4)
        );
        assert_eq!(
            rec.sessions[0].session_id, "s5",
            "the oldest fell off the front"
        );
    }

    #[test]
    fn bad_input_is_an_error_outcome_not_a_panic() {
        let r = rig();
        let env = FakeEnv::lane(100);
        for bad in ["", "{", "[]", r#"{"hook_event_name":"SessionStart"}"#] {
            let out = go(&r, &env, &Alive(vec![]), "SessionStart", None, bad);
            assert!(matches!(out, Outcome::Error(_)), "{bad:?} -> {out:?}");
        }
        let out = go(
            &r,
            &env,
            &Alive(vec![]),
            "PreToolUse",
            None,
            &payload("s1", None),
        );
        assert!(
            matches!(out, Outcome::Error(ref e) if e.contains("PreToolUse")),
            "{out:?}"
        );
        assert!(r.registry.list().unwrap().records.is_empty());
    }

    #[test]
    fn every_log_line_names_the_event_and_the_outcome() {
        let at = t(3);
        let id = AgentId::new("a1").unwrap();
        let l = Outcome::Registered {
            agent_id: id.clone(),
            created: true,
        }
        .log_line("SessionStart", at);
        assert!(l.contains("SessionStart ok registered a1"), "{l}");
        let l = Outcome::Skipped("headless".into()).log_line("SessionStart", at);
        assert!(l.contains("skipped: headless"), "{l}");
        let l = Outcome::Error("boom".into()).log_line("SessionEnd", at);
        assert!(l.contains("SessionEnd ERROR: boom"), "{l}");
        let l = Outcome::Ended { agent_id: id }.log_line("SessionEnd", at);
        assert!(l.contains("ok ended a1"), "{l}");
    }

    #[test]
    fn the_snapshot_serves_the_walk_lane_restart_expects_through_git_bash_layers() {
        // hook(40) <- bash(30) <- bash(20) <- claude(10) <- pwsh(5) <- WindowsTerminal(2)
        // This is the chain OverMind measured on a real interactive session.
        let snap = SnapshotParents::from_entries(&[
            (40, Some(30), "powershell.exe", 4040),
            (30, Some(20), "bash.exe", 3030),
            (20, Some(10), "bash.exe", 2020),
            (10, Some(5), "claude.exe", 1010),
            (5, Some(2), "pwsh.exe", 505),
            (2, None, "WindowsTerminal.exe", 202),
        ]);
        assert_eq!(snap.parent_of(40), Some((30, "bash.exe".to_string())));
        assert_eq!(snap.start_time_of(10), Some(1010));
        assert_eq!(snap.start_time_of(999), None);
        assert_eq!(snap.parent_of(2), None, "the root has no parent");
        assert_eq!(claude_parent_pid(40, &snap).unwrap(), 10);
        // A stranger between the hook and claude is refused, exactly as with the real source.
        let stranger = SnapshotParents::from_entries(&[
            (40, Some(30), "powershell.exe", 1),
            (30, Some(10), "node.exe", 1),
            (10, None, "claude.exe", 1),
        ]);
        assert!(claude_parent_pid(40, &stranger).is_err());
    }

    #[test]
    fn a_live_snapshot_finds_this_test_process_and_its_parent() {
        let snap = SnapshotParents::capture();
        let me = std::process::id();
        assert!(
            snap.start_time_of(me).is_some(),
            "this process is in the snapshot"
        );
        assert!(snap.parent_of(me).is_some(), "and so is its parent");
        let cmd = snap.cmdline_of(me).expect("own command line is readable");
        assert!(
            !cmd.is_empty(),
            "cmd is set explicitly, not left at sysinfo's empty default"
        );
    }
}
