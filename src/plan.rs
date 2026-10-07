// SPDX-License-Identifier: MIT OR Apache-2.0
//! The restore planner (M3a): from the registry and the process table to an exact, ordered,
//! hashable list of what *would* be started. **It starts nothing and writes nothing**; a plan is a
//! value. Design: `DESIGN.md` §2, `DESIGN-REVISION-1.md` §3 and §6, `DESIGN-REVISION-2.md` §3 and §5;
//! this module's own rules are in `docs/PLAN.md`.
//!
//! The rules, in the order they are applied to every registered agent:
//!
//! 1. **Candidate**: `intent = wanted` and not alive. Closed (parked or exited) and lazy agents are
//!    never candidates; an agent that is alive is never in a plan (idempotence); an agent whose
//!    liveness cannot be proved (no start time) is *not started*, because the failure direction is
//!    never to start a duplicate.
//! 2. **Hard rules**, independent of what the registry says: a recorded argv must exist; `cwd` must
//!    exist and be under the portfolio root; `bypassPermissions` only for the PM, otherwise the
//!    agent is **refused, not downgraded**; no `;` in any argv element. The argv is **rebuilt from an
//!    allowlist** of the recorded one: what is dropped is listed, never silent.
//! 3. **Order**: the PM first and alone; then the `priority` list; then the most recently active;
//!    then by name and id, so the same registry always gives the same plan.
//! 4. **Caps**: `max_running` counts agents already running; free memory below the floor holds
//!    everything. Held agents are reported as held, not as failed.
//! 5. **Pacing and placement**: batches of `batch_size` after the PM's; window `agentlife-<k>`,
//!    `tabs_per_window` tabs each. Placement is assigned here and never read back as a requirement.
//!
//! The plan carries a **hash** over everything that decides what is started. A frozen copy
//! (`freeze`) refuses to load if it was altered, and `check_unchanged` names what changed in the
//! registry since it was frozen.

use crate::atomic::write_atomic;
use crate::config::Config;
use crate::home::Home;
use crate::identity::{path_is_under, ProcessTable};
use crate::list::{liveness, Liveness};
use crate::marks::{pin_kind, PinKind};
use crate::registry::{AgentRecord, Intent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

pub const PLAN_SCHEMA: u32 = 1;

/// The measured median time for an agent to settle, used only for the estimate (`DESIGN-REVISION-1`
/// §6.3: a lower bound; the maximum observed was 123 s).
pub const SETTLE_SECS_ESTIMATE: u64 = 12;

/// The one development-channel value restore will pass on. Every other value is dropped and listed.
pub const ALLOWED_CHANNEL: &str = "server:claude-peers";

const CHANNEL_FLAG: &str = "--dangerously-load-development-channels";

/// Permission modes whose relation to each other is known (`DESIGN.md` §4.1). `plan` and anything
/// else is *incomparable*: the entry is flagged so a person looks at it.
const KNOWN_MODES: &[&str] = &["default", "acceptEdits", "auto", "bypassPermissions"];

/// Why an agent is not in the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Not selected by `--only`.
    NotSelected,
    /// Intent is closed or lazy, so restore leaves it alone.
    NotWanted,
    /// Already running; starting it again would be a duplicate.
    Running,
    /// Its process is alive but the record has no start time to prove it is the same one.
    Unverifiable,
    /// A live agent with the same name in the same directory exists.
    DuplicateAlive,
    /// The record carries no launch arguments, so nothing can be rebuilt.
    NoLaunchArgs,
    CwdMissing,
    CwdOutsideRoot,
    /// `bypassPermissions` for an agent that is not the PM: refused, never downgraded.
    BypassNotPm,
    /// A launch value that must not be passed (a `;`, a control character, or a flag shaped value).
    UnsafeArgument,
}

impl Reason {
    /// A reason a person is unlikely to need spelled out per agent (counted, not listed).
    pub fn is_routine(self) -> bool {
        matches!(
            self,
            Reason::NotSelected | Reason::NotWanted | Reason::Running
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub agent_id: String,
    pub name: Option<String>,
    pub reason: Reason,
    pub detail: String,
}

/// One agent that would be started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub agent_id: String,
    pub name: Option<String>,
    /// What the tab is called: the name, else the id.
    pub title: String,
    pub cwd: String,
    /// The rebuilt arguments, **without** the program name.
    pub argv: Vec<String>,
    pub mode: Option<String>,
    pub pm: bool,
    /// Things a person should look at (an unknown permission mode, a dropped channel).
    pub flags: Vec<String>,
    /// Recorded arguments that were not passed on, verbatim.
    pub dropped_args: Vec<String>,
    pub batch: u32,
    /// `agentlife-<window>`.
    pub window: u32,
    pub tab: u32,
}

/// An agent that is wanted and ready but not in this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub agent_id: String,
    pub name: Option<String>,
    /// `max_running` or `memory`.
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub batch_size: u32,
    pub batch_delay_secs: u64,
    pub liveness_timeout_secs: u64,
    pub tabs_per_window: u32,
    pub max_running: Option<u32>,
    pub free_ram_floor_gb: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub schema: u32,
    pub params: Params,
    /// Agents running when the plan was built (they count against `max_running`).
    pub running_now: u32,
    /// Free memory when the plan was built, if it could be read. Informational: not in the hash.
    pub free_ram_gb: Option<f64>,
    pub entries: Vec<Entry>,
    pub held: Vec<Held>,
    pub excluded: Vec<Excluded>,
    /// SHA-256 over what decides what is started; see [`Plan::compute_hash`].
    pub hash: String,
}

/// Everything [`build`] needs from outside. All of it is injected, so a test supplies each.
pub struct Inputs<'a> {
    pub records: &'a [AgentRecord],
    pub table: &'a dyn ProcessTable,
    pub cfg: &'a Config,
    pub free_ram_gb: Option<f64>,
    pub cwd_exists: &'a dyn Fn(&str) -> bool,
    /// Names or ids that start first after the PM, in this order.
    pub priority: &'a [String],
    /// If set, only these names or ids are considered.
    pub only: Option<&'a [String]>,
}

fn matches_any(rec: &AgentRecord, list: &[String]) -> Option<usize> {
    list.iter().position(|s| {
        rec.agent_id.as_str().eq_ignore_ascii_case(s)
            || rec
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(s))
    })
}

fn last_activity(rec: &AgentRecord) -> DateTime<Utc> {
    rec.sessions
        .last()
        .map(|s| s.ended_at.unwrap_or(s.started_at).max(s.started_at))
        .unwrap_or(rec.first_seen)
}

/// A value restore will pass on: non-empty, not shaped like a flag, no `;`, no control character.
fn safe_value(v: &str) -> bool {
    !v.is_empty() && !v.starts_with('-') && !v.contains(';') && !v.chars().any(char::is_control)
}

struct Rebuilt {
    argv: Vec<String>,
    mode: Option<String>,
    flags: Vec<String>,
    dropped: Vec<String>,
}

/// Rebuilds the argument list from the recorded one through an allowlist. Pure.
///
/// Kept: `--name`/`-n`, `--model`, `--permission-mode` (and `--dangerously-skip-permissions`, which
/// is the same mode), `--remote-control`, and the development-channel flag **only** with
/// [`ALLOWED_CHANNEL`] (without it a restarted lane can send but never receive claude-peers
/// messages). Everything else, including any prompt, is dropped and listed. The result is in one
/// canonical order, so two records that differ only in flag order give one plan entry.
fn rebuild(
    recorded: &[String],
    observed_mode: Option<&str>,
    is_pm: bool,
) -> Result<Rebuilt, (Reason, String)> {
    let mut name: Option<String> = None;
    let mut model: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut remote = false;
    let mut channel = false;
    let mut flags = Vec::new();
    let mut dropped = Vec::new();

    let mut it = recorded.iter().peekable();
    // The first element is the program when it is not a flag.
    if it.peek().is_some_and(|a| !a.starts_with('-')) {
        it.next();
    }
    let value_of = |flag: &str,
                    inline: Option<&str>,
                    it: &mut std::iter::Peekable<std::slice::Iter<String>>| {
        let v: Option<String> = match inline {
            Some(v) => Some(v.to_string()),
            None => it.next().cloned(),
        };
        v.ok_or_else(|| (Reason::UnsafeArgument, format!("{flag} has no value")))
    };
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--name" | "-n" => name = Some(value_of(flag, inline, &mut it)?),
            "--model" => model = Some(value_of(flag, inline, &mut it)?),
            "--permission-mode" => mode = Some(value_of(flag, inline, &mut it)?),
            "--dangerously-skip-permissions" => mode = Some("bypassPermissions".to_string()),
            "--remote-control" => remote = true,
            CHANNEL_FLAG => {
                let v = value_of(flag, inline, &mut it)?;
                if v == ALLOWED_CHANNEL {
                    channel = true;
                } else {
                    flags.push(format!("channel-not-allowlisted:{v}"));
                    dropped.push(format!("{CHANNEL_FLAG} {v}"));
                }
            }
            _ => dropped.push(arg.clone()),
        }
    }
    let mode = observed_mode.map(str::to_string).or(mode);
    if mode.as_deref() == Some("bypassPermissions") && !is_pm {
        return Err((
            Reason::BypassNotPm,
            "bypassPermissions is for the PM only; refused, not downgraded".into(),
        ));
    }
    if let Some(m) = mode.as_deref() {
        if !KNOWN_MODES.contains(&m) {
            flags.push(format!("mode-needs-a-person:{m}"));
        }
    }
    let mut argv = Vec::new();
    for (flag, v) in [("--name", &name), ("--model", &model)] {
        if let Some(v) = v {
            argv.push(flag.to_string());
            argv.push(v.clone());
        }
    }
    if let Some(m) = &mode {
        argv.push("--permission-mode".to_string());
        argv.push(m.clone());
    }
    if remote {
        argv.push("--remote-control".to_string());
    }
    if channel {
        argv.push(CHANNEL_FLAG.to_string());
        argv.push(ALLOWED_CHANNEL.to_string());
    }
    if let Some(bad) = argv
        .iter()
        .find(|a| a.contains(';') || a.chars().any(char::is_control))
    {
        return Err((
            Reason::UnsafeArgument,
            format!("argument {bad:?} is not allowed"),
        ));
    }
    for v in [&name, &model, &mode].into_iter().flatten() {
        if !safe_value(v) {
            return Err((
                Reason::UnsafeArgument,
                format!("value {v:?} is not allowed"),
            ));
        }
    }
    Ok(Rebuilt {
        argv,
        mode,
        flags,
        dropped,
    })
}

struct Candidate {
    rec: AgentRecord,
    rebuilt: Rebuilt,
    pm: bool,
    priority: usize,
    activity: DateTime<Utc>,
}

/// Builds the plan. Pure given its inputs; never starts or writes anything.
pub fn build(i: &Inputs) -> Plan {
    let cfg = i.cfg;
    let params = Params {
        batch_size: cfg.batch_size.max(1),
        batch_delay_secs: cfg.batch_delay_secs,
        liveness_timeout_secs: cfg.liveness_timeout_secs,
        tabs_per_window: cfg.tabs_per_window.max(1),
        max_running: cfg.max_running,
        free_ram_floor_gb: cfg.free_ram_floor_gb,
    };

    // Deterministic order of consideration, whatever order the registry listed them in.
    let mut records: Vec<&AgentRecord> = i.records.iter().collect();
    records.sort_by(|a, b| a.agent_id.as_str().cmp(b.agent_id.as_str()));

    let live: BTreeMap<&str, Liveness> = records
        .iter()
        .map(|r| (r.agent_id.as_str(), liveness(r, i.table).0))
        .collect();
    let alive_now = |r: &AgentRecord| {
        matches!(
            live[r.agent_id.as_str()],
            Liveness::Running | Liveness::Unverified
        )
    };
    let running_now = records.iter().filter(|r| alive_now(r)).count() as u32;

    let mut excluded = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut exclude = |r: &AgentRecord, reason: Reason, detail: String| {
        excluded.push(Excluded {
            agent_id: r.agent_id.to_string(),
            name: r.name.clone(),
            reason,
            detail,
        });
    };

    for r in &records {
        if let Some(only) = i.only {
            if matches_any(r, only).is_none() {
                exclude(r, Reason::NotSelected, "not named by --only".into());
                continue;
            }
        }
        if !matches!(r.intent, Intent::Wanted) {
            exclude(
                r,
                Reason::NotWanted,
                format!("intent is {}", intent_word(&r.intent)),
            );
            continue;
        }
        match live[r.agent_id.as_str()] {
            Liveness::Running => {
                exclude(r, Reason::Running, "already running".into());
                continue;
            }
            Liveness::Unverified => {
                exclude(
                    r,
                    Reason::Unverifiable,
                    "its process is alive but has no recorded start time, so it cannot be told apart from a stranger; not started".into(),
                );
                continue;
            }
            Liveness::Stopped => {}
        }
        let twin = r.name.as_deref().and_then(|n| {
            records.iter().find(|o| {
                o.agent_id != r.agent_id
                    && alive_now(o)
                    && o.name
                        .as_deref()
                        .is_some_and(|on| on.eq_ignore_ascii_case(n))
                    && crate::identity::paths_equal(&o.launch_cwd, &r.launch_cwd)
            })
        });
        if let Some(o) = twin {
            exclude(
                r,
                Reason::DuplicateAlive,
                format!(
                    "{} has the same name in the same directory and is running",
                    o.agent_id
                ),
            );
            continue;
        }
        let Some(args) = r.launch_args.as_ref().filter(|a| !a.is_empty()) else {
            exclude(
                r,
                Reason::NoLaunchArgs,
                "no recorded launch arguments".into(),
            );
            continue;
        };
        if !path_is_under(&r.launch_cwd, &cfg.portfolio_root) {
            exclude(
                r,
                Reason::CwdOutsideRoot,
                format!("{} is not under {}", r.launch_cwd, cfg.portfolio_root),
            );
            continue;
        }
        if !(i.cwd_exists)(&r.launch_cwd) {
            exclude(
                r,
                Reason::CwdMissing,
                format!("{} does not exist", r.launch_cwd),
            );
            continue;
        }
        let pm = pin_kind(r, cfg) == Some(PinKind::Rule);
        match rebuild(args, r.permission_mode.as_deref(), pm) {
            Ok(rebuilt) => candidates.push(Candidate {
                rec: (*r).clone(),
                rebuilt,
                pm,
                priority: matches_any(r, i.priority).unwrap_or(usize::MAX),
                activity: last_activity(r),
            }),
            Err((reason, detail)) => exclude(r, reason, detail),
        }
    }

    // Order: PM, then priority list, then most recently active, then name, then id.
    candidates.sort_by(|a, b| {
        b.pm.cmp(&a.pm)
            .then(a.priority.cmp(&b.priority))
            .then(b.activity.cmp(&a.activity))
            .then(a.rec.name.cmp(&b.rec.name))
            .then(a.rec.agent_id.as_str().cmp(b.rec.agent_id.as_str()))
    });

    // Caps. Memory below the floor holds everything; max_running counts what already runs.
    let memory_low = i.free_ram_gb.is_some_and(|g| g < params.free_ram_floor_gb);
    let capacity = params
        .max_running
        .map(|m| m.saturating_sub(running_now) as usize)
        .unwrap_or(usize::MAX);
    let mut held = Vec::new();
    let mut kept: Vec<Candidate> = Vec::new();
    for c in candidates {
        let why = if memory_low {
            Some("memory")
        } else if kept.len() >= capacity {
            Some("max_running")
        } else {
            None
        };
        match why {
            Some(w) => held.push(Held {
                agent_id: c.rec.agent_id.to_string(),
                name: c.rec.name.clone(),
                why: w.to_string(),
            }),
            None => kept.push(c),
        }
    }

    // Batches: the PM alone, then chunks of batch_size. Placement by position.
    let mut entries = Vec::with_capacity(kept.len());
    let pm_first = kept.first().is_some_and(|c| c.pm);
    for (idx, c) in kept.into_iter().enumerate() {
        let batch = if pm_first {
            if idx == 0 {
                0
            } else {
                1 + ((idx - 1) as u32 / params.batch_size)
            }
        } else {
            idx as u32 / params.batch_size
        };
        entries.push(Entry {
            agent_id: c.rec.agent_id.to_string(),
            name: c.rec.name.clone(),
            title: c
                .rec
                .name
                .clone()
                .unwrap_or_else(|| c.rec.agent_id.to_string()),
            cwd: c.rec.launch_cwd.clone(),
            argv: c.rebuilt.argv,
            mode: c.rebuilt.mode,
            pm: c.pm,
            flags: c.rebuilt.flags,
            dropped_args: c.rebuilt.dropped,
            batch,
            window: idx as u32 / params.tabs_per_window,
            tab: idx as u32 % params.tabs_per_window,
        });
    }

    let mut plan = Plan {
        schema: PLAN_SCHEMA,
        params,
        running_now,
        free_ram_gb: i.free_ram_gb,
        entries,
        held,
        excluded,
        hash: String::new(),
    };
    plan.hash = plan.compute_hash();
    plan
}

fn intent_word(i: &Intent) -> &'static str {
    match i {
        Intent::Wanted => "wanted",
        Intent::Lazy => "lazy",
        Intent::Closed { how, .. } => match how {
            crate::registry::ClosedHow::Parked => "parked",
            crate::registry::ClosedHow::Exited => "exited",
        },
    }
}

impl Plan {
    /// SHA-256 over the schema, the parameters, every entry and every held agent. **Not** over
    /// anything that varies without changing what would start (free memory, the exclusion list,
    /// the running count), so rebuilding an unchanged registry gives the same hash and any change
    /// to what would be launched changes it.
    pub fn compute_hash(&self) -> String {
        #[derive(Serialize)]
        struct Body<'a> {
            schema: u32,
            params: &'a Params,
            entries: &'a [Entry],
            held: &'a [Held],
        }
        let bytes = serde_json::to_vec(&Body {
            schema: self.schema,
            params: &self.params,
            entries: &self.entries,
            held: &self.held,
        })
        .expect("a plan serialises");
        let digest = Sha256::digest(&bytes);
        let mut out = String::with_capacity(64);
        for b in digest.iter() {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    pub fn hash_is_valid(&self) -> bool {
        self.hash == self.compute_hash()
    }

    pub fn batch_count(&self) -> u32 {
        self.entries.iter().map(|e| e.batch + 1).max().unwrap_or(0)
    }

    /// The design's start-time budget (`DESIGN-REVISION-1` §6.3), a lower bound.
    pub fn estimate_secs(&self) -> u64 {
        u64::from(self.batch_count()) * (self.params.batch_delay_secs + SETTLE_SECS_ESTIMATE)
    }
}

/// The frozen form on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frozen {
    pub built_at: DateTime<Utc>,
    pub plan: Plan,
}

#[derive(Debug)]
pub enum FrozenError {
    Io(String),
    Corrupt(String),
    /// The file parses but its hash does not match its content: it was changed after freezing.
    Altered {
        recorded: String,
        actual: String,
    },
    Schema(u32),
}

impl std::fmt::Display for FrozenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrozenError::Io(e) => write!(f, "cannot read the frozen plan: {e}"),
            FrozenError::Corrupt(e) => write!(f, "the frozen plan is not valid: {e}"),
            FrozenError::Altered { recorded, actual } => write!(
                f,
                "the frozen plan was altered after it was frozen (recorded hash {recorded}, content hashes to {actual}); refusing it"
            ),
            FrozenError::Schema(n) => write!(f, "the frozen plan has schema {n}, expected {PLAN_SCHEMA}"),
        }
    }
}

impl std::error::Error for FrozenError {}

/// Writes the plan to `<home>/plans/<UTC>-<hash prefix>.json` atomically and returns the path.
/// Nothing in M3a calls this from a command: freezing belongs to the consent step (M4).
pub fn freeze(home: &Home, plan: &Plan, now: DateTime<Utc>) -> std::io::Result<PathBuf> {
    let dir = home.plans_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "{}-{}.json",
        now.format("%Y%m%dT%H%M%S%.3fZ"),
        &plan.hash[..12.min(plan.hash.len())]
    ));
    let body = serde_json::to_vec_pretty(&Frozen {
        built_at: now,
        plan: plan.clone(),
    })
    .map_err(std::io::Error::other)?;
    write_atomic(&path, &body)?;
    Ok(path)
}

/// Loads a frozen plan and refuses it if its hash does not match its content.
pub fn load_frozen(path: &std::path::Path) -> Result<Frozen, FrozenError> {
    let text = std::fs::read_to_string(path).map_err(|e| FrozenError::Io(e.to_string()))?;
    let f: Frozen = serde_json::from_str(&text).map_err(|e| FrozenError::Corrupt(e.to_string()))?;
    if f.plan.schema != PLAN_SCHEMA {
        return Err(FrozenError::Schema(f.plan.schema));
    }
    let actual = f.plan.compute_hash();
    if actual != f.plan.hash {
        return Err(FrozenError::Altered {
            recorded: f.plan.hash.clone(),
            actual,
        });
    }
    Ok(f)
}

/// Compares a frozen plan with one rebuilt from the registry now. `Ok` only if they would start
/// exactly the same thing; otherwise one line per difference, naming the agent. This is the
/// check that stops an execution whose registry moved after the person saw the plan.
pub fn check_unchanged(frozen: &Plan, now: &Plan) -> Result<(), Vec<String>> {
    if frozen.hash == now.hash {
        return Ok(());
    }
    let mut diffs = Vec::new();
    let by_id = |p: &Plan| -> BTreeMap<String, Entry> {
        p.entries
            .iter()
            .map(|e| (e.agent_id.clone(), e.clone()))
            .collect()
    };
    let (a, b) = (by_id(frozen), by_id(now));
    let ids: HashSet<&String> = a.keys().chain(b.keys()).collect();
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    for id in ids {
        match (a.get(id), b.get(id)) {
            (Some(_), None) => diffs.push(format!("{id}: was in the plan, is not now")),
            (None, Some(_)) => diffs.push(format!("{id}: was not in the plan, is now")),
            (Some(x), Some(y)) if x != y => diffs.push(format!("{id}: its launch details changed")),
            _ => {}
        }
    }
    if diffs.is_empty() {
        diffs.push("the plan's parameters or its held list changed".into());
    }
    Err(diffs)
}

/// Text for a person. `dry_run` only changes the closing line.
pub fn render_text(p: &Plan, dry_run: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "restore plan {}: {} to start in {} batch(es), {} held, {} running now",
        &p.hash[..12.min(p.hash.len())],
        p.entries.len(),
        p.batch_count(),
        p.held.len(),
        p.running_now
    );
    let _ = writeln!(
        s,
        "  pacing: batches of {} every {} s (liveness bound {} s); about {} s at least",
        p.params.batch_size,
        p.params.batch_delay_secs,
        p.params.liveness_timeout_secs,
        p.estimate_secs()
    );
    let _ = writeln!(
        s,
        "  caps: max_running {}; memory floor {} GB, free now {}",
        p.params
            .max_running
            .map_or("none".to_string(), |m| m.to_string()),
        p.params.free_ram_floor_gb,
        p.free_ram_gb
            .map_or("unknown".to_string(), |g| format!("{g:.1} GB"))
    );
    let mut last_batch = u32::MAX;
    for e in &p.entries {
        if e.batch != last_batch {
            let _ = writeln!(s, "batch {}:", e.batch);
            last_batch = e.batch;
        }
        let _ = writeln!(
            s,
            "  {} {} [agentlife-{} tab {}] {} {}",
            if e.pm { "PM" } else { "  " },
            e.title,
            e.window,
            e.tab,
            e.cwd,
            e.argv.join(" ")
        );
        for f in &e.flags {
            let _ = writeln!(s, "      ! {f}");
        }
        for d in &e.dropped_args {
            let _ = writeln!(s, "      not passed on: {d}");
        }
    }
    for h in &p.held {
        let _ = writeln!(
            s,
            "held ({}): {}",
            h.why,
            h.name.as_deref().unwrap_or(&h.agent_id)
        );
    }
    let mut routine: BTreeMap<String, u32> = BTreeMap::new();
    for x in &p.excluded {
        if x.reason.is_routine() {
            *routine.entry(x.detail.clone()).or_default() += 1;
        } else {
            let _ = writeln!(
                s,
                "not started: {} ({:?}): {}",
                x.name.as_deref().unwrap_or(&x.agent_id),
                x.reason,
                x.detail
            );
        }
    }
    for (why, n) in &routine {
        let _ = writeln!(s, "left alone: {n} ({why})");
    }
    let _ = writeln!(
        s,
        "{}",
        if dry_run {
            "DRY RUN: nothing was started and nothing was written."
        } else {
            "This is a plan only; starting it needs a person's consent (not built yet)."
        }
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{ProcessIdentity, ProcessTable};
    use crate::registry::{AgentId, ClosedHow, Session};
    use chrono::TimeZone;
    use std::collections::HashMap;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    /// Which `(pid, start)` pairs are alive.
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

    fn none_alive() -> Alive {
        Alive(HashMap::new())
    }

    fn rec(id: &str, name: &str, args: &[&str], active: i64) -> AgentRecord {
        let mut r = AgentRecord::new(
            AgentId::new(id).unwrap(),
            format!("C:/Projects/{name}"),
            t(0),
        );
        r.name = Some(name.to_string());
        r.launch_args = Some(args.iter().map(|s| s.to_string()).collect());
        r.sessions.push(Session {
            session_id: format!("s-{id}"),
            pid: 1000 + id.len() as u32 + (active as u32 % 7),
            process_start_secs: Some(5000),
            started_at: t(active),
            ended_at: Some(t(active + 1)),
            end_reason: Some("other".into()),
        });
        r
    }

    fn std_args(name: &str) -> Vec<String> {
        [
            "claude",
            "--name",
            name,
            "--dangerously-load-development-channels",
            "server:claude-peers",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn lane(id: &str, name: &str, active: i64) -> AgentRecord {
        let mut r = rec(id, name, &[], active);
        r.launch_args = Some(std_args(name));
        r
    }

    fn pm_rec(active: i64) -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new("a-pm").unwrap(), "C:/Projects", t(0));
        r.name = Some("pm".into());
        r.launch_args = Some(vec![
            "claude".into(),
            "--name".into(),
            "pm".into(),
            "--dangerously-skip-permissions".into(),
        ]);
        r.permission_mode = Some("bypassPermissions".into());
        r.sessions.push(Session {
            session_id: "s-pm".into(),
            pid: 4,
            process_start_secs: Some(5000),
            started_at: t(active),
            ended_at: Some(t(active + 1)),
            end_reason: None,
        });
        r
    }

    fn cfg() -> Config {
        Config::default()
    }

    fn plan_of(records: &[AgentRecord], table: &Alive, c: &Config) -> Plan {
        build(&Inputs {
            records,
            table,
            cfg: c,
            free_ram_gb: Some(32.0),
            cwd_exists: &|_| true,
            priority: &[],
            only: None,
        })
    }

    fn names(p: &Plan) -> Vec<String> {
        p.entries.iter().map(|e| e.title.clone()).collect()
    }

    #[test]
    fn the_pm_is_first_and_alone_then_batches_of_batch_size_most_recent_first() {
        let mut recs = vec![pm_rec(10)];
        for (i, n) in ["a", "b", "c", "d", "e", "f", "g"].iter().enumerate() {
            recs.push(lane(&format!("a-{n}"), n, 100 + i as i64));
        }
        let p = plan_of(&recs, &none_alive(), &cfg());
        assert_eq!(names(&p), ["pm", "g", "f", "e", "d", "c", "b", "a"]);
        let batches: Vec<u32> = p.entries.iter().map(|e| e.batch).collect();
        assert_eq!(batches, [0, 1, 1, 1, 2, 2, 2, 3]);
        assert_eq!(p.batch_count(), 4);
        assert!(p.entries[0].pm && p.entries[1..].iter().all(|e| !e.pm));
    }

    #[test]
    fn without_a_pm_batches_start_at_zero() {
        let recs = vec![lane("a-a", "a", 1), lane("a-b", "b", 2)];
        let p = plan_of(&recs, &none_alive(), &cfg());
        assert_eq!(
            p.entries.iter().map(|e| e.batch).collect::<Vec<_>>(),
            [0, 0]
        );
    }

    #[test]
    fn an_explicit_pin_does_not_make_a_lane_the_pm_and_so_does_not_order_it_first() {
        let mut other = lane("a-x", "x", 500);
        other.pinned = Some(crate::registry::Pin {
            by: "person".into(),
            at: t(0),
        });
        let recs = vec![other, pm_rec(10)];
        let p = plan_of(&recs, &none_alive(), &cfg());
        assert_eq!(names(&p), ["pm", "x"]);
        assert!(!p.entries[1].pm);
    }

    #[test]
    fn the_priority_list_comes_after_the_pm_and_before_recency() {
        let recs = vec![
            pm_rec(1),
            lane("a-new", "new", 900),
            lane("a-old", "old", 1),
            lane("a-mid", "mid", 500),
        ];
        let p = build(&Inputs {
            records: &recs,
            table: &none_alive(),
            cfg: &cfg(),
            free_ram_gb: None,
            cwd_exists: &|_| true,
            priority: &["old".to_string(), "mid".to_string()],
            only: None,
        });
        assert_eq!(names(&p), ["pm", "old", "mid", "new"]);
    }

    #[test]
    fn an_agent_that_is_alive_is_never_in_the_plan_and_counts_against_max_running() {
        let mut up = lane("a-up", "up", 5);
        up.sessions[0].ended_at = None;
        up.sessions[0].pid = 77;
        let recs = vec![
            up,
            lane("a-x", "x", 4),
            lane("a-y", "y", 3),
            lane("a-z", "z", 2),
        ];
        let table = Alive(HashMap::from([(77, 5000)]));
        let mut c = cfg();
        c.max_running = Some(3);
        let p = plan_of(&recs, &table, &c);
        assert_eq!(p.running_now, 1);
        assert_eq!(names(&p), ["x", "y"], "3 allowed, 1 already running");
        assert_eq!(
            p.held,
            [Held {
                agent_id: "a-z".into(),
                name: Some("z".into()),
                why: "max_running".into()
            }]
        );
        assert!(p
            .excluded
            .iter()
            .any(|x| x.agent_id == "a-up" && x.reason == Reason::Running));
    }

    #[test]
    fn a_recycled_pid_with_a_different_start_time_is_not_alive() {
        let mut r = lane("a-x", "x", 5);
        r.sessions[0].ended_at = None;
        r.sessions[0].pid = 77;
        let table = Alive(HashMap::from([(77, 9999)]));
        let p = plan_of(&[r], &table, &cfg());
        assert_eq!(
            names(&p),
            ["x"],
            "same pid, other process: the agent is down"
        );
    }

    #[test]
    fn an_alive_process_with_no_recorded_start_time_is_not_started() {
        let mut r = lane("a-x", "x", 5);
        r.sessions[0].ended_at = None;
        r.sessions[0].pid = 77;
        r.sessions[0].process_start_secs = None;
        let p = plan_of(&[r], &Alive(HashMap::from([(77, 1)])), &cfg());
        assert!(p.entries.is_empty());
        assert_eq!(p.excluded[0].reason, Reason::Unverifiable);
    }

    #[test]
    fn closed_and_lazy_agents_are_never_candidates_and_the_reason_says_which() {
        let mut parked = lane("a-p", "parked", 1);
        parked.intent = Intent::Closed {
            how: ClosedHow::Parked,
            by: "person".into(),
            at: t(0),
        };
        let mut exited = lane("a-e", "exited", 2);
        exited.intent = Intent::Closed {
            how: ClosedHow::Exited,
            by: "person".into(),
            at: t(0),
        };
        let mut lazy = lane("a-l", "lazy", 3);
        lazy.intent = Intent::Lazy;
        let p = plan_of(
            &[parked, exited, lazy, lane("a-w", "wanted", 4)],
            &none_alive(),
            &cfg(),
        );
        assert_eq!(names(&p), ["wanted"]);
        let details: Vec<_> = p.excluded.iter().map(|x| x.detail.as_str()).collect();
        for want in ["intent is parked", "intent is exited", "intent is lazy"] {
            assert!(details.contains(&want), "{details:?}");
        }
    }

    #[test]
    fn a_twin_of_a_live_agent_is_not_started() {
        let mut up = lane("a-up", "same", 5);
        up.sessions[0].ended_at = None;
        up.sessions[0].pid = 77;
        let twin = lane("a-twin", "same", 4);
        let p = plan_of(&[up, twin], &Alive(HashMap::from([(77, 5000)])), &cfg());
        assert!(p.entries.is_empty());
        assert!(p
            .excluded
            .iter()
            .any(|x| x.agent_id == "a-twin" && x.reason == Reason::DuplicateAlive));
    }

    #[test]
    fn memory_below_the_floor_holds_everything_as_held_not_failed() {
        let recs = vec![pm_rec(1), lane("a-x", "x", 2)];
        let p = build(&Inputs {
            records: &recs,
            table: &none_alive(),
            cfg: &cfg(),
            free_ram_gb: Some(7.9),
            cwd_exists: &|_| true,
            priority: &[],
            only: None,
        });
        assert!(p.entries.is_empty());
        assert_eq!(p.held.len(), 2);
        assert!(p.held.iter().all(|h| h.why == "memory"));
        // At exactly the floor it is fine, and with the reading unavailable nothing is held.
        for g in [Some(8.0), None] {
            let p = build(&Inputs {
                records: &recs,
                table: &none_alive(),
                cfg: &cfg(),
                free_ram_gb: g,
                cwd_exists: &|_| true,
                priority: &[],
                only: None,
            });
            assert_eq!(p.entries.len(), 2, "{g:?}");
        }
    }

    #[test]
    fn bypass_is_for_the_pm_only_and_is_refused_not_downgraded() {
        let mut not_pm = lane("a-x", "x", 1);
        not_pm.launch_args = Some(
            ["claude", "--name", "x", "--dangerously-skip-permissions"]
                .map(String::from)
                .to_vec(),
        );
        let mut observed = lane("a-y", "y", 2);
        observed.permission_mode = Some("bypassPermissions".into());
        let p = plan_of(&[not_pm, observed, pm_rec(3)], &none_alive(), &cfg());
        assert_eq!(names(&p), ["pm"]);
        assert_eq!(p.entries[0].mode.as_deref(), Some("bypassPermissions"));
        let refused: Vec<_> = p
            .excluded
            .iter()
            .filter(|x| x.reason == Reason::BypassNotPm)
            .map(|x| x.agent_id.as_str())
            .collect();
        assert_eq!(refused, ["a-x", "a-y"]);
    }

    #[test]
    fn a_pm_look_alike_outside_the_portfolio_root_is_not_the_pm_so_bypass_is_refused() {
        let mut fake = pm_rec(1);
        fake.agent_id = AgentId::new("a-fake").unwrap();
        fake.launch_cwd = "C:/Projects/somewhere".into();
        let p = plan_of(&[fake], &none_alive(), &cfg());
        assert!(p.entries.is_empty());
        assert_eq!(p.excluded[0].reason, Reason::BypassNotPm);
    }

    #[test]
    fn a_semicolon_or_control_character_in_a_passed_value_refuses_the_agent() {
        for bad in ["a;b", "a\nb", "-x", ""] {
            let mut r = lane("a-x", "x", 1);
            r.launch_args = Some(["claude", "--name", bad].map(String::from).to_vec());
            let p = plan_of(&[r], &none_alive(), &cfg());
            assert!(p.entries.is_empty(), "{bad:?}");
            assert_eq!(p.excluded[0].reason, Reason::UnsafeArgument, "{bad:?}");
        }
    }

    #[test]
    fn a_dropped_argument_is_listed_never_silent_and_a_foreign_channel_is_flagged() {
        let mut r = lane("a-x", "x", 1);
        r.launch_args = Some(
            [
                "claude",
                "--name=x",
                "--model",
                "opus",
                "--add-dir",
                "C:/secret",
                "--dangerously-load-development-channels",
                "server:evil",
                "do the thing; rm -rf /",
                "--remote-control",
            ]
            .map(String::from)
            .to_vec(),
        );
        let p = plan_of(&[r], &none_alive(), &cfg());
        let e = &p.entries[0];
        assert_eq!(
            e.argv,
            ["--name", "x", "--model", "opus", "--remote-control"]
        );
        assert_eq!(
            e.dropped_args,
            [
                "--add-dir",
                "C:/secret",
                "--dangerously-load-development-channels server:evil",
                "do the thing; rm -rf /"
            ]
        );
        assert_eq!(e.flags, ["channel-not-allowlisted:server:evil"]);
        assert!(e.argv.iter().all(|a| !a.contains(';')));
    }

    #[test]
    fn the_peers_channel_is_kept_and_the_argv_has_one_canonical_order() {
        let a = lane("a-x", "x", 1);
        let mut b = lane("a-x", "x", 1);
        b.launch_args = Some(
            [
                "claude",
                "--dangerously-load-development-channels",
                "server:claude-peers",
                "-n",
                "x",
            ]
            .map(String::from)
            .to_vec(),
        );
        let (pa, pb) = (
            plan_of(&[a], &none_alive(), &cfg()),
            plan_of(&[b], &none_alive(), &cfg()),
        );
        assert_eq!(pa.entries[0].argv, pb.entries[0].argv);
        assert_eq!(
            pa.entries[0].argv,
            [
                "--name",
                "x",
                "--dangerously-load-development-channels",
                "server:claude-peers"
            ]
        );
        assert_eq!(pa.hash, pb.hash);
    }

    #[test]
    fn plan_mode_and_unknown_modes_are_flagged_for_a_person() {
        for m in ["plan", "somethingNew"] {
            let mut r = lane("a-x", "x", 1);
            r.permission_mode = Some(m.into());
            let p = plan_of(&[r], &none_alive(), &cfg());
            assert_eq!(p.entries[0].flags, [format!("mode-needs-a-person:{m}")]);
        }
        let mut r = lane("a-x", "x", 1);
        r.permission_mode = Some("auto".into());
        assert!(plan_of(&[r], &none_alive(), &cfg()).entries[0]
            .flags
            .is_empty());
    }

    #[test]
    fn the_observed_mode_wins_over_the_launch_flag_so_a_restart_never_widens() {
        // Launched with the bypass flag but observed running in `auto`: it comes back as `auto`.
        let mut r = lane("a-pm2", "pm", 1);
        r.launch_cwd = "C:/Projects".into();
        r.launch_args = Some(
            ["claude", "--name", "pm", "--dangerously-skip-permissions"]
                .map(String::from)
                .to_vec(),
        );
        r.permission_mode = Some("auto".into());
        let p = plan_of(&[r], &none_alive(), &cfg());
        assert_eq!(p.entries[0].mode.as_deref(), Some("auto"));
        assert_eq!(
            p.entries[0].argv,
            ["--name", "pm", "--permission-mode", "auto"]
        );
    }

    #[test]
    fn who_is_held_is_part_of_the_hash_even_when_the_entries_are_the_same() {
        // Same parameters, same two entries; only the held agent differs.
        let two = vec![lane("a-x", "x", 3), lane("a-y", "y", 2)];
        let mut three = two.clone();
        three.push(lane("a-z", "z", 1));
        let c = Config {
            max_running: Some(2),
            ..cfg()
        };
        let (p2, p3) = (
            plan_of(&two, &none_alive(), &c),
            plan_of(&three, &none_alive(), &c),
        );
        assert_eq!(p2.entries, p3.entries);
        assert_eq!((p2.held.len(), p3.held.len()), (0, 1));
        assert_ne!(p2.hash, p3.hash);
    }

    #[test]
    fn a_zero_batch_size_or_tabs_per_window_is_treated_as_one_and_never_divides_by_zero() {
        let recs = vec![lane("a-x", "x", 3), lane("a-y", "y", 2)];
        let c = Config {
            batch_size: 0,
            tabs_per_window: 0,
            ..cfg()
        };
        let p = plan_of(&recs, &none_alive(), &c);
        assert_eq!(p.params.batch_size, 1);
        assert_eq!(p.params.tabs_per_window, 1);
        assert_eq!(
            p.entries.iter().map(|e| e.batch).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            p.entries.iter().map(|e| e.window).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn cwd_must_exist_and_be_under_the_portfolio_root() {
        let mut outside = lane("a-o", "o", 1);
        outside.launch_cwd = "C:/Windows/System32".into();
        let mut dotdot = lane("a-d", "d", 2);
        dotdot.launch_cwd = "C:/Projects/../Windows".into();
        let gone = lane("a-g", "g", 3);
        let ok = lane("a-k", "k", 4);
        let p = build(&Inputs {
            records: &[outside, dotdot, gone, ok],
            table: &none_alive(),
            cfg: &cfg(),
            free_ram_gb: None,
            cwd_exists: &|c| !c.ends_with("/g"),
            priority: &[],
            only: None,
        });
        assert_eq!(names(&p), ["k"]);
        let why: BTreeMap<_, _> = p
            .excluded
            .iter()
            .map(|x| (x.agent_id.as_str(), x.reason))
            .collect();
        assert_eq!(why["a-o"], Reason::CwdOutsideRoot);
        assert_eq!(why["a-d"], Reason::CwdOutsideRoot);
        assert_eq!(why["a-g"], Reason::CwdMissing);
    }

    #[test]
    fn a_record_with_no_launch_args_cannot_be_rebuilt() {
        let mut r = lane("a-x", "x", 1);
        r.launch_args = None;
        let p = plan_of(&[r], &none_alive(), &cfg());
        assert_eq!(p.excluded[0].reason, Reason::NoLaunchArgs);
    }

    #[test]
    fn only_selects_by_name_or_id_case_insensitively_and_the_rest_are_not_selected() {
        let recs = vec![
            lane("a-x", "alpha", 1),
            lane("a-y", "beta", 2),
            lane("a-z", "gamma", 3),
        ];
        let p = build(&Inputs {
            records: &recs,
            table: &none_alive(),
            cfg: &cfg(),
            free_ram_gb: None,
            cwd_exists: &|_| true,
            priority: &[],
            only: Some(&["ALPHA".to_string(), "a-z".to_string()]),
        });
        assert_eq!(names(&p), ["gamma", "alpha"]);
        assert_eq!(
            p.excluded
                .iter()
                .filter(|x| x.reason == Reason::NotSelected)
                .count(),
            1
        );
    }

    #[test]
    fn windows_hold_tabs_per_window_agents_and_are_numbered_in_launch_order() {
        let recs: Vec<_> = (0..25)
            .map(|i| lane(&format!("a-{i:02}"), &format!("n{i:02}"), i))
            .collect();
        let p = plan_of(&recs, &none_alive(), &cfg());
        let pos: Vec<(u32, u32)> = p.entries.iter().map(|e| (e.window, e.tab)).collect();
        assert_eq!(pos[0], (0, 0));
        assert_eq!(pos[9], (0, 9));
        assert_eq!(pos[10], (1, 0));
        assert_eq!(pos[24], (2, 4));
        let mut c = cfg();
        c.tabs_per_window = 4;
        let p = plan_of(&recs, &none_alive(), &c);
        assert_eq!(p.entries[24].window, 6);
    }

    #[test]
    fn the_hash_is_stable_across_rebuilds_and_input_order_and_changes_with_any_launch_field() {
        let recs = vec![pm_rec(1), lane("a-x", "x", 2), lane("a-y", "y", 3)];
        let h = plan_of(&recs, &none_alive(), &cfg()).hash;
        assert_eq!(h.len(), 64);
        let mut rev = recs.clone();
        rev.reverse();
        assert_eq!(plan_of(&rev, &none_alive(), &cfg()).hash, h);
        // Free memory and the running count do not change what would start, so not the hash.
        let p = build(&Inputs {
            records: &recs,
            table: &none_alive(),
            cfg: &cfg(),
            free_ram_gb: Some(9999.0),
            cwd_exists: &|_| true,
            priority: &[],
            only: None,
        });
        assert_eq!(p.hash, h);
        // Anything that changes a launch does.
        let mut m = recs.clone();
        m[1].launch_args = Some(
            ["claude", "--name", "x", "--model", "m"]
                .map(String::from)
                .to_vec(),
        );
        assert_ne!(plan_of(&m, &none_alive(), &cfg()).hash, h);
        let mut m = recs.clone();
        m[2].launch_cwd = "C:/Projects/elsewhere".into();
        assert_ne!(plan_of(&m, &none_alive(), &cfg()).hash, h);
        let mut c = cfg();
        c.batch_size = 2;
        assert_ne!(plan_of(&recs, &none_alive(), &c).hash, h);
        assert!(plan_of(&recs, &none_alive(), &c).hash_is_valid());
    }

    #[test]
    fn a_frozen_plan_round_trips_and_an_edited_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path()).unwrap();
        let plan = plan_of(&[pm_rec(1), lane("a-x", "x", 2)], &none_alive(), &cfg());
        let path = freeze(&home, &plan, t(0)).unwrap();
        assert!(path.starts_with(home.plans_dir()));
        assert_eq!(load_frozen(&path).unwrap().plan, plan);
        // Change one argument in the file, leave the recorded hash alone.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.matches("\"--name\"").count(),
            2,
            "anchor present twice"
        );
        let edited = text.replacen("\"x\"", "\"y\"", 1);
        assert_ne!(edited, text, "the edit applied");
        std::fs::write(&path, edited).unwrap();
        assert!(matches!(
            load_frozen(&path),
            Err(FrozenError::Altered { .. })
        ));
        // Not JSON at all.
        std::fs::write(&path, "nope").unwrap();
        assert!(matches!(load_frozen(&path), Err(FrozenError::Corrupt(_))));
    }

    #[test]
    fn a_registry_that_moved_after_freezing_is_refused_and_the_agent_is_named() {
        let recs = vec![pm_rec(1), lane("a-x", "x", 2), lane("a-y", "y", 3)];
        let frozen = plan_of(&recs, &none_alive(), &cfg());
        assert_eq!(check_unchanged(&frozen, &frozen.clone()), Ok(()));
        // One agent's launch details changed.
        let mut changed = recs.clone();
        changed[1].launch_args = Some(
            ["claude", "--name", "x", "--model", "m"]
                .map(String::from)
                .to_vec(),
        );
        let diffs =
            check_unchanged(&frozen, &plan_of(&changed, &none_alive(), &cfg())).unwrap_err();
        assert_eq!(diffs, ["a-x: its launch details changed"]);
        // One agent gone, one new.
        let mut moved = recs.clone();
        moved.remove(2);
        moved.push(lane("a-new", "new", 9));
        let diffs = check_unchanged(&frozen, &plan_of(&moved, &none_alive(), &cfg())).unwrap_err();
        assert!(
            diffs.contains(&"a-y: was in the plan, is not now".to_string()),
            "{diffs:?}"
        );
        assert!(
            diffs.contains(&"a-new: was not in the plan, is now".to_string()),
            "{diffs:?}"
        );
    }

    #[test]
    fn the_text_says_what_will_start_what_was_dropped_and_that_nothing_was_started() {
        let mut r = lane("a-x", "x", 1);
        r.launch_args = Some(
            ["claude", "--name", "x", "--add-dir", "C:/d"]
                .map(String::from)
                .to_vec(),
        );
        let mut parked = lane("a-p", "p", 2);
        parked.intent = Intent::Lazy;
        let p = plan_of(&[pm_rec(1), r, parked], &none_alive(), &cfg());
        let s = render_text(&p, true);
        for want in [
            "2 to start in 2 batch(es)",
            "PM pm [agentlife-0 tab 0]",
            "not passed on: --add-dir",
            "left alone: 1 (intent is lazy)",
            "DRY RUN: nothing was started and nothing was written.",
        ] {
            assert!(s.contains(want), "missing {want:?} in\n{s}");
        }
    }
}
