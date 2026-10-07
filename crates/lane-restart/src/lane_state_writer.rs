// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart state <event>` - the hook command that maintains
//! `C:/Projects/.lane-state/<role>.json`. RESTART-TOOL-DESIGN.md §10.
//!
//! ⚠️ A NATIVE SUBCOMMAND, NOT A POWERSHELL SCRIPT. PM finding, 2026-09-18:
//! spawning `powershell.exe` plus `Get-CimInstance` on every `PreToolUse`
//! (i.e. every tool call, in every lane) cost ~0.3-1s each - a portfolio-
//! wide latency tax. This binary starts in single-digit milliseconds and
//! needs no shell at all, closing that cost and the earlier `"shell":
//! "powershell"` version-ambiguity question (§10's PowerShell-7-only
//! `-AsUTC` concern) in the same move.

use crate::state::LaneState;
use chrono::Utc;
use serde::Deserialize;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// PM finding, 2026-09-18 (fourth interactive retest): `model` IS present in
/// a real `SessionStart` payload's key list - but only the key names were
/// logged, not the value's shape. ⚠️ NOT CONFIRMED: this crate's own headless
/// probe (an ephemeral `claude -p` session) doesn't include `model` in
/// `SessionStart` at all, the same headless/interactive gap §10.3 already
/// found once - so the shape below is a defensive guess, not a verified
/// fact, until a real interactive capture confirms it. Accepts either a
/// plain string or an object carrying an `id` field, so a future confirmed
/// shape doesn't need a schema-breaking follow-up either way.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ModelField {
    Plain(String),
    WithId { id: String },
}

impl ModelField {
    fn into_string(self) -> String {
        match self {
            ModelField::Plain(s) => s,
            ModelField::WithId { id } => id,
        }
    }
}

/// The subset of hooks.md's documented common input fields this command
/// reads. Unknown fields are ignored by `serde_json`'s default behaviour -
/// no `deny_unknown_fields` here, since a future Claude Code version adding
/// fields must not break this parse.
///
/// ⚠️ REVISED (PM finding, 2026-09-18, third interactive retest): a real
/// `SessionStart` payload does NOT include `permission_mode` - parsing
/// failed with "missing field `permission_mode`" against live input, not a
/// hypothetical. "Documented common field" is not the same claim as
/// "present on every event, always" - hooks.md's own table never promised
/// that. Only `hook_event_name`, `session_id`, and `cwd` are still required;
/// everything else this struct reads is `Option<T>`, absent rather than
/// guessed when an event's real payload doesn't include it.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    pub cwd: String,
    pub permission_mode: Option<String>,
    pub model: Option<ModelField>,
    /// `~/.claude/projects/<launch dir, encoded>/<session_id>.jsonl` - the
    /// one field that still names the LAUNCH directory after the session
    /// has moved on, used only to cross-check which cwd to record.
    pub transcript_path: Option<String>,
}

/// RESTART-TOOL-DESIGN.md §10.2, PM finding 2026-09-18: the cwd-leaf default
/// gives the PM's own lane "projects" (its cwd is `C:/Projects` itself).
/// `LANE_ROLE`, when set, always wins; the cwd leaf is the fallback for
/// every lane whose directory name already IS its role.
pub fn resolve_role(cwd: &str, lane_role_env: Option<&str>) -> String {
    if let Some(r) = lane_role_env {
        if !r.is_empty() {
            return r.to_string();
        }
    }
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| cwd.to_lowercase())
}

#[derive(Debug)]
pub enum PidError {
    ParentNotFound,
    /// A non-shell, non-`claude` image appeared before `claude` was found -
    /// refused rather than skipped, unlike a shell hop.
    UnexpectedAncestor(String),
    /// Walked `MAX_HOPS` shell layers without finding `claude` - refused
    /// rather than walking forever.
    HopLimitExceeded,
}

impl std::fmt::Display for PidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PidError::ParentNotFound => write!(f, "could not find this hook's parent process"),
            PidError::UnexpectedAncestor(name) => {
                write!(
                    f,
                    "ancestor process is {name:?} - neither claude nor a known shell, \
                     refusing to record a wrong pid"
                )
            }
            PidError::HopLimitExceeded => {
                write!(
                    f,
                    "no claude process found within {MAX_ANCESTRY_HOPS} shell hops"
                )
            }
        }
    }
}

/// The `claude` process's own pid, from the CURRENTLY RUNNING HOOK's
/// ancestry - hooks.md's common input fields do NOT include one directly
/// (RESTART-TOOL-DESIGN.md §10.1), so this derives it instead of trusting
/// anything the hook input claims.
pub trait ParentProcess {
    /// `(parent_pid, parent_image_name)` of the process identified by `pid`.
    fn parent_of(&self, pid: u32) -> Option<(u32, String)>;

    /// The full argv of the process identified by `pid`, if it could be
    /// read. Used only against the `claude` pid `claude_parent_pid` already
    /// found - never against an unverified process.
    fn cmdline_of(&self, pid: u32) -> Option<Vec<String>>;

    /// When the process identified by `pid` started, in seconds since the
    /// epoch, if it could be read.
    fn start_time_of(&self, _pid: u32) -> Option<u64> {
        None
    }
}

/// What `claude`'s own launch command line says, that no hook field
/// carries. PM finding, 2026-09-18 (fourth interactive retest): a session
/// launched with `--remote-control` still recorded `remote_control: false`,
/// because no `hooks.md` field reports it at all. Read from the pid
/// `claude_parent_pid` already verified, not guessed and not left as
/// another "unknown, treated as safe" gap.
///
/// ⚠️ KNOWN LIMIT (PM finding, 2026-09-18, post-merge gate read): CireSnave's
/// own settings carry `remoteControlAtStartup: true`, so a lane can get
/// Remote Control with no `--remote-control` flag on its command line at
/// all - `remote_control` then reads `false` here even though the session
/// genuinely has it. Not wrong *in practice*: a relaunch picks Remote
/// Control back up from that same setting regardless of what this field
/// says. But the state file itself doesn't prove it either way. Low
/// priority - RESTART-TOOL-DESIGN.md §10.1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeCliFlags {
    pub remote_control: bool,
    /// `--name`, `--name=` or `-n`: how a person recognises the session.
    pub name: Option<String>,
    pub permission_mode: Option<String>,
    /// The full argv this was parsed from, retained verbatim - PM finding,
    /// 2026-09-18 (CireSnave, via the PM): a relaunch that only reconstructs
    /// `--model`/`--permission-mode`/`--remote-control` silently drops
    /// every other real launch flag (CireSnave's own lanes always carry
    /// `--dangerously-load-development-channels server:claude-peers`,
    /// without which a relaunched lane can send but never RECEIVE
    /// `claude-peers` notifications). `main.rs`'s relaunch rebuilds its
    /// own argv from an ALLOWLIST parsed out of this, never passed
    /// through blindly.
    pub launch_args: Option<Vec<String>>,
}

/// Pure - given a command line, no I/O. `--dangerously-skip-permissions`
/// maps to Claude Code's own name for that mode (`bypassPermissions`,
/// confirmed in `sessions.md`'s permission-mode table), not a guessed
/// string. Accepts `--permission-mode value` and `--permission-mode=value`
/// both (PM finding, 2026-09-18, post-merge gate read: the equals form
/// wasn't parsed).
pub fn parse_claude_cli_flags(cmdline: &[String]) -> ClaudeCliFlags {
    let mut flags = ClaudeCliFlags {
        launch_args: Some(cmdline.to_vec()),
        ..ClaudeCliFlags::default()
    };
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        if let Some(v) = arg.strip_prefix("--permission-mode=") {
            flags.permission_mode = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--name=") {
            flags.name = Some(v.to_string());
            continue;
        }
        match arg.as_str() {
            "--remote-control" => flags.remote_control = true,
            "--name" | "-n" => {
                if let Some(v) = iter.next() {
                    flags.name = Some(v.clone());
                }
            }
            "--dangerously-skip-permissions" => {
                flags.permission_mode = Some("bypassPermissions".to_string())
            }
            "--permission-mode" => {
                if let Some(v) = iter.next() {
                    flags.permission_mode = Some(v.clone());
                }
            }
            _ => {}
        }
    }
    flags
}

/// Image names (lowercased, no `.exe`) a hop is allowed to pass through
/// without stopping. PM finding, 2026-09-18, from a REAL interactive
/// session: Claude Code ran a shell-form hook command through Git Bash,
/// two layers deep (`hook <- bash <- bash <- claude.exe`) - the DIRECT
/// parent was `bash.exe`, not `claude.exe`, so the single-hop check this
/// module shipped with (verified only against a headless `claude -p`
/// session, §10.3) refused every real interactive write.
const SHELL_ANCESTOR_NAMES: &[&str] = &["bash", "sh", "cmd", "powershell", "pwsh"];

/// How many shell layers this walk tolerates before giving up -
/// RESTART-TOOL-DESIGN.md §11.1's real chain needed 2; this leaves margin
/// without walking indefinitely.
const MAX_ANCESTRY_HOPS: u32 = 4;

fn image_base(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Walks up from `my_pid`, skipping ONLY known shell images, and returns
/// the pid of the first `claude` found. Refuses (never skips past) any
/// ancestor that is neither a known shell nor `claude` - a stranger
/// appearing before `claude` is exactly what this check exists to catch,
/// not something to tolerate the way a shell hop is tolerated.
pub fn claude_parent_pid(my_pid: u32, lookup: &dyn ParentProcess) -> Result<u32, PidError> {
    let mut current = my_pid;
    for _ in 0..MAX_ANCESTRY_HOPS {
        let (parent_pid, name) = lookup.parent_of(current).ok_or(PidError::ParentNotFound)?;
        let base = image_base(&name);
        if base == "claude" {
            return Ok(parent_pid);
        }
        if !SHELL_ANCESTOR_NAMES.contains(&base.as_str()) {
            return Err(PidError::UnexpectedAncestor(name));
        }
        current = parent_pid;
    }
    Err(PidError::HopLimitExceeded)
}

/// `path` and each of its ancestors, nearest first, on either separator -
/// not `Path::ancestors`, which on a non-Windows host would not split a
/// Windows path.
fn self_and_ancestors(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut p = path.trim_end_matches(['/', '\\']);
    while !p.is_empty() {
        out.push(p);
        match p.rfind(['/', '\\']) {
            Some(i) => p = &p[..i],
            None => break,
        }
    }
    out
}

/// Every state file in `state_dir` that parses, with the role its FILE
/// name says (the name is what a later write targets, whatever the file's
/// own `role` field claims).
fn all_states(state_dir: &Path) -> Vec<(String, LaneState)> {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let role = name.strip_suffix(".json")?.to_string();
            let state = crate::state::load(state_dir, &role).ok()?;
            Some((role, state))
        })
        .collect()
}

/// Which state file a hook event belongs to: the lane's, wherever its
/// session has wandered. PM task, 2026-10-04: naming it from the event's cwd
/// leaf sent a lane that cd'd into a subdirectory, a sibling worktree or an
/// `EnterWorktree` worktree to `<that dir>.json`.
///
/// In order:
/// 1. `LANE_ROLE`.
/// 2. The file already recording THIS session and THIS hook's verified
///    `claude` pid - true wherever the lane has gone, and unlike the
///    transcript path it does not move: Claude Code moves a session's
///    transcript into the worktree's project dir on `EnterWorktree`
///    (measured 2026-10-04, review of #0). If files the old naming left
///    carry the same session and pid, the one with the shortest recorded
///    cwd: the launch dir is above, or beside, where the lane wandered.
/// 3. For a session's first event: the event cwd or its nearest ancestor
///    whose encoding matches the transcript path's directory (a lossy
///    encoding: it can confirm a candidate, never be decoded into one).
/// 4. The event cwd's leaf, as before.
pub fn hook_role(
    state_dir: &Path,
    input: &HookInput,
    lane_role_env: Option<&str>,
    claude_pid: u32,
) -> String {
    if let Some(r) = lane_role_env.filter(|r| !r.is_empty()) {
        return r.to_string();
    }
    if let Some((role, _)) = all_states(state_dir)
        .into_iter()
        .filter(|(_, s)| s.session_id == input.session_id && s.pid == claude_pid)
        .min_by(|(ra, a), (rb, b)| a.cwd.len().cmp(&b.cwd.len()).then_with(|| ra.cmp(rb)))
    {
        return role;
    }
    if let Some(dir) = input
        .transcript_path
        .as_deref()
        .and_then(transcript_project_dir)
    {
        if let Some(launch) = self_and_ancestors(&input.cwd)
            .into_iter()
            .find(|a| crate::paths::project_dir_name(a).eq_ignore_ascii_case(dir))
        {
            return resolve_role(launch, None);
        }
    }
    resolve_role(&input.cwd, None)
}

/// Which state file `assert-idle` belongs to. It has no hook input, only
/// its own process: `LANE_ROLE`; else the most recently updated state file
/// recording the caller's own `claude` pid (files the old naming left
/// behind carry the same pid, but only the live one keeps being updated);
/// else the cwd's leaf, as before - and `run_assert_idle` then refuses that
/// file unless it records the caller's own pid.
fn assert_idle_role(
    state_dir: &Path,
    claude_pid: Option<u32>,
    lane_role_env: Option<&str>,
    cwd: &str,
) -> String {
    if let Some(r) = lane_role_env.filter(|r| !r.is_empty()) {
        return r.to_string();
    }
    claude_pid
        .and_then(|pid| {
            all_states(state_dir)
                .into_iter()
                .filter(|(_, s)| s.pid == pid)
                .max_by_key(|(_, s)| s.updated_at)
        })
        .map(|(role, _)| role)
        .unwrap_or_else(|| resolve_role(cwd, None))
}

/// The parent directory's own name in a transcript path, either separator.
fn transcript_project_dir(transcript_path: &str) -> Option<&str> {
    let mut parts = transcript_path.rsplit(['/', '\\']);
    parts.next()?;
    parts.next().filter(|d| !d.is_empty())
}

/// The cwd a state file records: the lane's HOME (its launch directory),
/// which `decide`'s transcript check, the relaunch and the liveness check
/// all key on - never merely where the session happens to be right now.
/// PM finding, 2026-10-02: `SessionStart` also fires on a compaction,
/// mid-session, with the CURRENT cwd, and recording that sent the PM's own
/// restart looking in the wrong project directory.
///
/// The candidates, in order: the cwd already recorded by this SAME process
/// (a process's launch directory never changes), then this event's own cwd.
/// When `transcript_path` is present, the first candidate whose encoding
/// matches the transcript's own directory wins. When none matches, the
/// order alone decides and nothing is guessed - `decide`'s transcript check
/// then refuses rather than relaunching in the wrong place.
pub fn recorded_cwd(existing: Option<&LaneState>, input: &HookInput, pid: u32) -> String {
    let candidates: Vec<&str> = existing
        .filter(|s| s.pid == pid)
        .map(|s| s.cwd.as_str())
        .into_iter()
        .chain(std::iter::once(input.cwd.as_str()))
        .collect();
    let by_transcript = input
        .transcript_path
        .as_deref()
        .and_then(transcript_project_dir)
        .and_then(|dir| {
            candidates
                .iter()
                .find(|c| crate::paths::project_dir_name(c).eq_ignore_ascii_case(dir))
        });
    by_transcript.unwrap_or(&candidates[0]).to_string()
}

/// A fresh baseline state, built exactly the way `SessionStart` builds one -
/// PM finding, 2026-09-19: lanes already running when the user-level hooks
/// were installed (synapse; the PM, via `--resume`) have no state file at
/// all, because every event before this fix was silently ignored without an
/// existing `SessionStart`. `model` stays `None` here deliberately - it's
/// genuinely unknown (no hook field reliably carries it outside
/// `SessionStart`'s own payload, and nothing here guesses), so a relaunch
/// built from this state omits `--model` and falls back to the user's own
/// default (Sonnet), never a wrong invented one. ⚠️ IDENTITY SAFETY IS THE
/// CALLER'S: `pid` here must already be `claude_parent_pid`'s verified
/// result, never called speculatively - `run()`'s own ordering (walk the
/// ancestry, THEN bootstrap) is what keeps that true, not anything in this
/// function.
fn bootstrap_state(
    role: &str,
    input: &HookInput,
    pid: u32,
    cwd: String,
    cli_flags: &ClaudeCliFlags,
    event: &str,
    now: chrono::DateTime<Utc>,
) -> LaneState {
    LaneState {
        role: role.to_string(),
        session_id: input.session_id.clone(),
        pid,
        pid_start_secs: None,
        cwd,
        name: cli_flags.name.clone(),
        model: input.model.clone().map(ModelField::into_string),
        permission_mode: input
            .permission_mode
            .clone()
            .or_else(|| cli_flags.permission_mode.clone()),
        remote_control: cli_flags.remote_control,
        launch_args: cli_flags.launch_args.clone(),
        busy: false,
        subagents_running: 0,
        no_background_shells: None,
        updated_at: now,
        updated_by_event: event.to_string(),
    }
}

/// Computes the next `LaneState` for `event`, given whatever state already
/// existed (if any). Pure - no file I/O, so every branch is directly
/// testable. `SessionEnd` returns `None`: the caller deletes the file.
pub fn apply_event(
    existing: Option<LaneState>,
    event: &str,
    input: &HookInput,
    role: &str,
    pid: u32,
    cli_flags: &ClaudeCliFlags,
    now: chrono::DateTime<Utc>,
) -> Option<LaneState> {
    if event == "SessionStart" {
        return Some(LaneState {
            role: role.to_string(),
            session_id: input.session_id.clone(),
            pid,
            pid_start_secs: None,
            cwd: recorded_cwd(existing.as_ref(), input, pid),
            // The same "always fresh" rule as `remote_control` when this
            // launch's command line was read; kept only when it was not.
            name: if cli_flags
                .launch_args
                .as_ref()
                .is_some_and(|a| !a.is_empty())
            {
                cli_flags.name.clone()
            } else {
                existing.as_ref().and_then(|s| s.name.clone())
            },
            // ⚠️ Prefer THIS event's own value (PM finding, 2026-09-18: a
            // real SessionStart DOES carry `model`); fall back to what a
            // prior SessionStart already learned, never invent one when
            // neither source has it.
            model: input
                .model
                .clone()
                .map(ModelField::into_string)
                .or_else(|| existing.as_ref().and_then(|s| s.model.clone())),
            // Prefer the JSON field if a future Claude Code version ever
            // sends one; then the launch command line's own flag; then
            // whatever an earlier SessionStart already learned. Never
            // invent one when none of the three has it (PM: "don't invent
            // a value" - RESTART-TOOL-DESIGN.md §10.4).
            permission_mode: input
                .permission_mode
                .clone()
                .or_else(|| cli_flags.permission_mode.clone())
                .or_else(|| existing.as_ref().and_then(|s| s.permission_mode.clone())),
            // ⚠️ Always the CLI flags read fresh from THIS launch's own
            // command line, never preserved from a prior state - PM
            // finding, 2026-09-18: no hook field carries this at all, and a
            // stale carried-forward value would be exactly as wrong as
            // inventing one if this launch's real flags disagree with it.
            remote_control: cli_flags.remote_control,
            // Same "always fresh, never preserved" rule as `remote_control`
            // above, for the same reason: a stale argv from a prior launch
            // would be exactly as wrong as inventing one if this launch's
            // real command line disagrees with it.
            launch_args: cli_flags.launch_args.clone(),
            busy: false,
            subagents_running: 0,
            no_background_shells: None,
            updated_at: now,
            updated_by_event: event.to_string(),
        });
    }
    if event == "SessionEnd" {
        return None;
    }

    // ⚠️ BOOTSTRAP, PM finding 2026-09-19: a lane already running when the
    // hooks were installed has no state file, and every event before this
    // was silently a no-op (`existing?` returned `None`) - blocking the
    // first real use for exactly the lanes already mid-session. A session
    // whose `session_id` no longer matches this event's own input is
    // treated the same way (a genuinely new session under a state file this
    // module never saw start) - not merged with stale busy/subagent counts
    // from whatever session the old file was actually about.
    let cwd = recorded_cwd(existing.as_ref(), input, pid);
    let mut state = match existing {
        // PM task, 2026-10-03: the SAME process takes `recorded_cwd`'s
        // answer on every event, not only at `SessionStart`, so a cwd
        // recorded wrong before that fix heals instead of lasting the life
        // of the process. For the same pid it moves only on transcript
        // proof; another pid's event never moves it.
        Some(mut s) if s.session_id == input.session_id => {
            if s.pid == pid {
                s.cwd = cwd;
            }
            s
        }
        _ => bootstrap_state(role, input, pid, cwd, cli_flags, event, now),
    };
    // A state a real `SessionStart` created BEFORE this fix never recorded
    // `launch_args` at all (the PM's own `pm.json`, via `--resume`) -
    // opportunistically refreshed here, on any later event, rather than
    // left permanently empty until the lane's next real `SessionStart`.
    if state.launch_args.is_none() {
        if let Some(launch_args) = &cli_flags.launch_args {
            state.launch_args = Some(launch_args.clone());
        }
    }
    if state.name.is_none() {
        state.name = cli_flags.name.clone();
    }
    state.updated_at = now;
    state.updated_by_event = event.to_string();
    match event {
        "UserPromptSubmit" | "PreToolUse" => {
            state.busy = true;
            // ⚠️ PM finding, 2026-09-18: a stale `AssertIdle` must not
            // authorize a LATER restart once the lane has done more work
            // that could have started a background shell since asserting -
            // the assertion is a claim about THIS moment, not a durable
            // fact. Cleared here, not just left to the state file's own
            // staleness window, which only bounds how OLD an assertion may
            // be, not whether real work happened after it.
            state.no_background_shells = None;
        }
        "Stop" => state.busy = false,
        "SubagentStart" => state.subagents_running += 1,
        "SubagentStop" => state.subagents_running = state.subagents_running.saturating_sub(1),
        // §1a: the lane's own assertion, made by running `lane-restart
        // assert-idle` itself right after writing HANDOFF - never inferred
        // from any hook event, since no hook can honestly know whether a
        // background shell is still running.
        "AssertIdle" => state.no_background_shells = Some(true),
        "PostModelSwitch" => {
            if let Some(m) = &input.model {
                state.model = Some(m.clone().into_string());
            }
        }
        _ => {}
    }
    Some(state)
}

#[derive(Debug)]
pub enum WriteError {
    LockTimedOut,
    Io(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::LockTimedOut => write!(f, "timed out waiting for the lane-state lock"),
            WriteError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// ⚠️ PM finding, 2026-09-19 (install, `hook-errors.log`, two real "timed
/// out waiting for the lane-state lock" errors around the install): the
/// production call sites had `timeout=2s` SHORTER than `stale_after=5s` -
/// if a holder dies (crashed or killed hook) mid-lock, every waiter gives
/// up at 2s, before anyone is even ALLOWED to reclaim the abandoned lock at
/// 5s, dropping the event outright. `timeout` must exceed `stale_after` so
/// a waiter can actually live long enough to perform the reclaim.
pub const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(7);
pub const LOCK_STALE_AFTER: Duration = Duration::from_secs(5);

/// A lock a caller holds for the duration of one read-modify-write cycle.
/// PM finding, 2026-09-18: concurrent tool calls fire concurrent hooks, and
/// an unguarded read-modify-write on one JSON file loses updates - a lost
/// `SubagentStart` makes `subagents_running` too LOW, which is the unsafe
/// direction (it can make a busy lane look idle). A create-new-file lock
/// is atomic at the OS level: exactly one of two racing callers succeeds.
pub struct StateLock {
    path: PathBuf,
}

impl StateLock {
    /// Blocks until the lock is acquired or `timeout` elapses. A lock file
    /// older than `stale_after` is treated as abandoned (a crashed holder
    /// that never released it) and removed before retrying - a stale lock
    /// must not wedge every future hook invocation forever.
    pub fn acquire(
        lock_path: PathBuf,
        timeout: Duration,
        stale_after: Duration,
    ) -> Result<Self, WriteError> {
        let deadline = Instant::now() + timeout;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Self { path: lock_path }),
                // ⚠️ WINDOWS: a `create_new` racing a concurrent `remove_file` (another
                // holder's `Drop`, running right now) can surface as `PermissionDenied`
                // instead of `AlreadyExists` while the file is mid-deletion - found live
                // in CI, not assumed. Both mean the same thing here: someone else has
                // this lock busy right now, retry.
                Err(e)
                    if e.kind() == ErrorKind::AlreadyExists
                        || e.kind() == ErrorKind::PermissionDenied =>
                {
                    if let Ok(meta) = std::fs::metadata(&lock_path) {
                        if let Ok(age) = SystemTime::now()
                            .duration_since(meta.modified().unwrap_or(SystemTime::now()))
                        {
                            if age > stale_after {
                                let _ = std::fs::remove_file(&lock_path);
                                continue;
                            }
                        }
                    }
                    if Instant::now() > deadline {
                        return Err(WriteError::LockTimedOut);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(WriteError::Io(e.to_string())),
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn write_atomic(path: &Path, state: &LaneState) -> Result<(), WriteError> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state).map_err(|e| WriteError::Io(e.to_string()))?;
    std::fs::write(&tmp, json).map_err(|e| WriteError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| WriteError::Io(e.to_string()))?;
    Ok(())
}

/// Records when the state's `claude` process started, once per process: a
/// pid alone can be reused by the OS. Only for the pid this hook itself
/// verified (`claude_parent_pid`), never another one a state carries.
fn with_start_time(mut state: LaneState, pid: u32, lookup: &dyn ParentProcess) -> LaneState {
    if state.pid == pid && state.pid_start_secs.is_none() {
        state.pid_start_secs = lookup.start_time_of(pid);
    }
    state
}

/// The whole hook invocation: read stdin, lock, read-modify-write, unlock.
pub fn run(
    state_dir: &Path,
    event: &str,
    my_pid: u32,
    parent_lookup: &dyn ParentProcess,
    lane_role_env: Option<&str>,
    stdin: &mut dyn Read,
) -> Result<(), String> {
    let mut buf = String::new();
    stdin
        .read_to_string(&mut buf)
        .map_err(|e| format!("could not read hook input: {e}"))?;
    let input: HookInput =
        serde_json::from_str(&buf).map_err(|e| format!("could not parse hook input: {e}"))?;

    // The verified claude pid first: it is what names this process's file.
    let pid = claude_parent_pid(my_pid, parent_lookup).map_err(|e| e.to_string())?;
    let role = hook_role(state_dir, &input, lane_role_env, pid);
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let path = state_dir.join(format!("{role}.json"));
    let lock = StateLock::acquire(
        state_dir.join(format!("{role}.lock")),
        LOCK_ACQUIRE_TIMEOUT,
        LOCK_STALE_AFTER,
    )
    .map_err(|e| e.to_string())?;

    let existing = crate::state::load(state_dir, &role).ok();
    // ⚠️ Only ever read against the pid `claude_parent_pid` already
    // verified - never an unvetted process. Missing/unreadable cmdline is
    // "nothing detected", not a hard failure of the whole hook.
    let cli_flags = parent_lookup
        .cmdline_of(pid)
        .map(|cmd| parse_claude_cli_flags(&cmd))
        .unwrap_or_default();
    let next = apply_event(existing, event, &input, &role, pid, &cli_flags, Utc::now())
        .map(|state| with_start_time(state, pid, parent_lookup));

    let result = match next {
        Some(state) => write_atomic(&path, &state).map_err(|e| e.to_string()),
        None => {
            let _ = std::fs::remove_file(&path);
            Ok(())
        }
    };
    drop(lock);
    result
}

/// `lane-restart assert-idle` - run by the lane ITSELF, from its own shell,
/// right after writing HANDOFF. Not a hook: nothing invokes this on the
/// lane's behalf, and no hook payload backs it, because no hook can
/// honestly know whether a background shell the lane started is still
/// running (RESTART-TOOL-DESIGN.md §1a) - only the lane asserting it about
/// itself can. ⚠️ PM finding, 2026-09-18: without this command, no
/// production path ever wrote `no_background_shells: Some(true)` at all -
/// every real restart refused, and 79 passing tests didn't catch it because
/// every fixture hard-coded the field.
///
/// Requires a state file to already exist (a `SessionStart` this session) -
/// asserting idleness for a session `lane-restart` has never recorded makes
/// no sense, and is refused rather than silently creating one.
pub fn run_assert_idle(
    state_dir: &Path,
    my_pid: u32,
    parent_lookup: &dyn ParentProcess,
    lane_role_env: Option<&str>,
    cwd: &str,
) -> Result<(), String> {
    let caller = claude_parent_pid(my_pid, parent_lookup).ok();
    let role = assert_idle_role(state_dir, caller, lane_role_env, cwd);
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let path = state_dir.join(format!("{role}.json"));
    let lock = StateLock::acquire(
        state_dir.join(format!("{role}.lock")),
        LOCK_ACQUIRE_TIMEOUT,
        LOCK_STALE_AFTER,
    )
    .map_err(|e| e.to_string())?;

    let existing = crate::state::load(state_dir, &role).ok();
    if existing.is_none() {
        drop(lock);
        return Err(format!(
            "{role}: no state file - assert-idle requires a session that \
             has already recorded SessionStart"
        ));
    }
    let pid = claude_parent_pid(my_pid, parent_lookup).map_err(|e| e.to_string())?;
    if let Some(s) = existing.as_ref().filter(|s| s.pid != pid) {
        drop(lock);
        return Err(format!(
            "{role}: the state file records pid {}, not this lane's claude pid {pid} - \
             assert-idle only asserts about its own process",
            s.pid
        ));
    }
    let cli_flags = parent_lookup
        .cmdline_of(pid)
        .map(|cmd| parse_claude_cli_flags(&cmd))
        .unwrap_or_default();
    // ⚠️ The placeholder MUST carry the existing state's own session_id
    // (found 2026-09-27, live): `apply_event` treats any other session_id
    // as a new session and bootstraps a fresh state - an empty one here
    // rewrote the file with `session_id: ""`, so the restart that follows
    // in the same shell failed its transcript identity check every time.
    // The assertion is about the session already on record, never a new one.
    let existing_session_id = existing
        .as_ref()
        .map(|s| s.session_id.clone())
        .unwrap_or_default();
    let placeholder_input = HookInput {
        hook_event_name: "AssertIdle".to_string(),
        session_id: existing_session_id,
        cwd: cwd.to_string(),
        permission_mode: None,
        model: None,
        transcript_path: None,
    };
    let next = apply_event(
        existing,
        "AssertIdle",
        &placeholder_input,
        &role,
        pid,
        &cli_flags,
        Utc::now(),
    );
    let result = match next.map(|state| with_start_time(state, pid, parent_lookup)) {
        Some(state) => write_atomic(&path, &state).map_err(|e| e.to_string()),
        None => Err(format!("{role}: assert-idle produced no state to write")),
    };
    drop(lock);
    result
}

/// Real parent-process lookup, via the same `sysinfo` crate `facts.rs` uses.
/// ⚠️ Not unit tested here - `SysinfoFacts`'s own docs explain why: it needs
/// a real process, which this crate must never spin up just to test itself.
/// `apply_event`, `resolve_role`, `StateLock` and `write_atomic` carry the
/// real test coverage; this is exercised by actually running the hook.
pub struct RealParentProcess;

impl ParentProcess for RealParentProcess {
    fn start_time_of(&self, pid: u32) -> Option<u64> {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let pid = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        sys.process(pid).map(|p| p.start_time())
    }

    fn parent_of(&self, my_pid: u32) -> Option<(u32, String)> {
        use sysinfo::{Pid, System};
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let me = sys.process(Pid::from_u32(my_pid))?;
        let parent_pid = me.parent()?;
        let parent = sys.process(parent_pid)?;
        Some((
            parent_pid.as_u32(),
            parent.name().to_string_lossy().to_string(),
        ))
    }

    fn cmdline_of(&self, pid: u32) -> Option<Vec<String>> {
        use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};
        // ⚠️ PM finding, 2026-09-18 (first real restart attempt): plain
        // `refresh_processes` leaves `cmd` at `UpdateKind::Never` by default
        // (confirmed by reading sysinfo 0.39.6's own default impl) - this
        // was reading an always-empty command line, not a genuinely absent
        // one, which is why `remote_control` read `false` even on a session
        // launched with `--remote-control` on its real command line. Same
        // root cause, and the same fix, as `facts.rs`'s `cwd_of`.
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        let process = sys.process(Pid::from_u32(pid))?;
        Some(
            process
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn input(cwd: &str) -> HookInput {
        HookInput {
            hook_event_name: "Test".to_string(),
            session_id: "s-123".to_string(),
            cwd: cwd.to_string(),
            permission_mode: Some("prompting".to_string()),
            model: None,
            transcript_path: None,
        }
    }

    fn input_without_permission_mode(cwd: &str) -> HookInput {
        HookInput {
            permission_mode: None,
            ..input(cwd)
        }
    }

    // -- resolve_role ------------------------------------------------------- //

    #[test]
    fn role_env_var_wins_over_cwd_leaf() {
        assert_eq!(resolve_role("C:/Projects", Some("pm")), "pm");
    }

    #[test]
    fn cwd_leaf_is_the_fallback() {
        assert_eq!(resolve_role("C:/Projects/OverMind", None), "overmind");
    }

    #[test]
    fn an_empty_role_env_var_falls_back_too() {
        assert_eq!(resolve_role("C:/Projects/OverMind", Some("")), "overmind");
    }

    #[test]
    fn the_pm_case_that_found_this_bug() {
        // PM finding, 2026-09-18: without an override this reads "projects".
        assert_eq!(resolve_role("C:/Projects", None), "projects");
        assert_eq!(resolve_role("C:/Projects", Some("pm")), "pm");
    }

    // -- claude_parent_pid ----------------------------------------------------- //
    // PM finding, 2026-09-18, from a REAL interactive session: the direct parent
    // was `bash.exe`, not `claude.exe` - `hook <- bash <- bash <- claude.exe`.
    // Every case here uses a pid-keyed chain so a fake can express that shape,
    // not just a single hop.

    struct FakeAncestry(std::collections::HashMap<u32, (u32, String)>);
    impl FakeAncestry {
        fn chain(links: &[(u32, u32, &str)]) -> Self {
            let mut map = std::collections::HashMap::new();
            for (pid, parent_pid, parent_name) in links {
                map.insert(*pid, (*parent_pid, parent_name.to_string()));
            }
            Self(map)
        }
    }
    impl ParentProcess for FakeAncestry {
        fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }
        fn cmdline_of(&self, _pid: u32) -> Option<Vec<String>> {
            None
        }
    }

    #[test]
    fn accepts_a_direct_claude_parent() {
        let lookup = FakeAncestry::chain(&[(1, 99, "claude")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn accepts_claude_exe_case_insensitively() {
        let lookup = FakeAncestry::chain(&[(1, 99, "Claude.EXE")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn walks_past_shell_layers_to_find_claude() {
        // The PM's own observed chain, exactly: hook(1) <- bash(10) <- bash(20) <- claude(99).
        let lookup =
            FakeAncestry::chain(&[(1, 10, "bash"), (10, 20, "bash"), (20, 99, "claude.exe")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn walks_past_a_single_powershell_layer_too() {
        let lookup = FakeAncestry::chain(&[(1, 10, "powershell.exe"), (10, 99, "claude")]);
        assert_eq!(claude_parent_pid(1, &lookup).unwrap(), 99);
    }

    #[test]
    fn refuses_a_non_shell_non_claude_ancestor() {
        // The PM's own example: a stranger appearing before claude is found
        // must refuse, never be walked past the way a shell hop is.
        let lookup = FakeAncestry::chain(&[(1, 99, "node")]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::UnexpectedAncestor(_))
        ));
    }

    #[test]
    fn refuses_a_non_shell_ancestor_even_behind_a_real_shell_hop() {
        let lookup = FakeAncestry::chain(&[(1, 10, "bash"), (10, 99, "node")]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::UnexpectedAncestor(_))
        ));
    }

    #[test]
    fn refuses_when_the_hop_limit_is_exceeded() {
        // All shells, never reaching claude within MAX_ANCESTRY_HOPS.
        let lookup = FakeAncestry::chain(&[
            (1, 10, "bash"),
            (10, 20, "bash"),
            (20, 30, "bash"),
            (30, 40, "bash"),
            (40, 50, "bash"),
        ]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::HopLimitExceeded)
        ));
    }

    #[test]
    fn refuses_when_the_parent_cannot_be_found_at_all() {
        let lookup = FakeAncestry::chain(&[]);
        assert!(matches!(
            claude_parent_pid(1, &lookup),
            Err(PidError::ParentNotFound)
        ));
    }

    // -- parse_claude_cli_flags ------------------------------------------------ //
    // PM finding, 2026-09-18 (fourth interactive retest): no hook field
    // carries `remote_control`, and `permission_mode` isn't reliably present
    // either - both are read from the launch command line instead.

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn remote_control_flag_is_detected() {
        let cmdline = strs(&["claude.exe", "--remote-control"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: true,
                name: None,
                permission_mode: None,
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn permission_mode_value_is_extracted() {
        let cmdline = strs(&["claude.exe", "--permission-mode", "prompting"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: false,
                name: None,
                permission_mode: Some("prompting".to_string()),
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn permission_mode_equals_value_form_is_extracted_too() {
        // PM finding, 2026-09-18 (post-merge, PR #61's own gate read): the
        // space-separated form isn't the only one Claude Code's own CLI
        // accepts - `--permission-mode=value` must parse the same way.
        let cmdline = strs(&["claude.exe", "--permission-mode=bypassPermissions"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline).permission_mode,
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn dangerously_skip_permissions_maps_to_bypass_permissions() {
        let cmdline = strs(&["claude.exe", "--dangerously-skip-permissions"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline).permission_mode,
            Some("bypassPermissions".to_string())
        );
    }

    #[test]
    fn no_relevant_flags_leaves_both_fields_at_their_defaults() {
        // `--name` became relevant (it is recorded now); `--verbose` is not.
        let cmdline = strs(&["claude.exe", "--verbose"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                launch_args: Some(cmdline.clone()),
                ..ClaudeCliFlags::default()
            }
        );
    }

    #[test]
    fn flags_combine() {
        let cmdline = strs(&[
            "claude.exe",
            "--remote-control",
            "--permission-mode",
            "bypassPermissions",
        ]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                remote_control: true,
                name: None,
                permission_mode: Some("bypassPermissions".to_string()),
                launch_args: Some(cmdline.clone()),
            }
        );
    }

    #[test]
    fn a_trailing_permission_mode_flag_with_no_value_is_ignored_not_a_panic() {
        let cmdline = strs(&["claude.exe", "--permission-mode"]);
        assert_eq!(
            parse_claude_cli_flags(&cmdline),
            ClaudeCliFlags {
                launch_args: Some(cmdline.clone()),
                ..ClaudeCliFlags::default()
            }
        );
    }

    // -- apply_event ---------------------------------------------------------- //

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn session_start_creates_a_fresh_state_busy_false_zero_subagents() {
        let state = apply_event(
            None,
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(state.role, "overmind");
        assert_eq!(state.pid, 42);
        assert!(!state.busy);
        assert_eq!(state.subagents_running, 0);
        assert_eq!(state.no_background_shells, None);
    }

    #[test]
    fn session_start_with_no_permission_mode_does_not_invent_one() {
        // The exact real-world case, PM finding 2026-09-18: a live
        // SessionStart payload had no permission_mode field at all.
        let state = apply_event(
            None,
            "SessionStart",
            &input_without_permission_mode("C:/Projects/OverMind"),
            "overmind",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(
            state.permission_mode, None,
            "must be None, never a guessed default"
        );
    }

    #[test]
    fn session_start_preserves_a_previously_learned_permission_mode_across_a_resume() {
        let mut prior = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        prior.permission_mode = Some("bypassPermissions".to_string());
        let restarted = apply_event(
            Some(prior),
            "SessionStart",
            &input_without_permission_mode("C:/x"),
            "overmind",
            2,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(
            restarted.permission_mode,
            Some("bypassPermissions".to_string()),
            "a later SessionStart missing the field must not erase what was already known"
        );
    }

    /// ⚠️ THE REAL-WORLD FAILURE, REPRODUCED AS A PARSE, NOT JUST A STRUCT
    /// LITERAL: a struct built by hand in Rust can't prove the JSON parser
    /// itself tolerates a missing field the way the code above assumes.
    #[test]
    fn a_real_session_start_payload_missing_permission_mode_parses_cleanly() {
        let json = r#"{
            "hook_event_name": "SessionStart",
            "session_id": "abc-123",
            "cwd": "C:/Projects/OverMind"
        }"#;
        let parsed: HookInput = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.permission_mode, None);
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.hook_event_name, "SessionStart");
    }

    #[test]
    fn session_start_preserves_a_previously_learned_model_across_a_resume() {
        let prior = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        let mut prior = prior;
        prior.model = Some("claude-opus-5".to_string());
        let restarted = apply_event(
            Some(prior),
            "SessionStart",
            &input("C:/x"),
            "overmind",
            2,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(restarted.model, Some("claude-opus-5".to_string()));
    }

    #[test]
    fn session_start_reads_model_from_a_plain_string_input_field() {
        let mut with_model = input("C:/x");
        with_model.model = Some(ModelField::Plain("claude-sonnet-5".to_string()));
        let state = apply_event(
            None,
            "SessionStart",
            &with_model,
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(state.model, Some("claude-sonnet-5".to_string()));
    }

    #[test]
    fn session_start_reads_model_from_an_object_with_id_input_field() {
        // ⚠️ NOT CONFIRMED (see ModelField's own doc comment): the real shape
        // is still unverified, so both plausible shapes must parse.
        let mut with_model = input("C:/x");
        with_model.model = Some(ModelField::WithId {
            id: "claude-sonnet-5".to_string(),
        });
        let state = apply_event(
            None,
            "SessionStart",
            &with_model,
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(state.model, Some("claude-sonnet-5".to_string()));
    }

    #[test]
    fn session_start_falls_back_to_cli_permission_mode_when_the_field_is_absent() {
        let cli_flags = ClaudeCliFlags {
            remote_control: false,
            name: None,
            permission_mode: Some("bypassPermissions".to_string()),
            launch_args: None,
        };
        let state = apply_event(
            None,
            "SessionStart",
            &input_without_permission_mode("C:/x"),
            "overmind",
            1,
            &cli_flags,
            now(),
        )
        .unwrap();
        assert_eq!(
            state.permission_mode,
            Some("bypassPermissions".to_string()),
            "the launch command line is the fallback source, not just a JSON field"
        );
    }

    #[test]
    fn session_start_prefers_the_input_field_over_the_cli_flag_when_both_are_present() {
        let cli_flags = ClaudeCliFlags {
            remote_control: false,
            name: None,
            permission_mode: Some("bypassPermissions".to_string()),
            launch_args: None,
        };
        // `input()` carries permission_mode: Some("prompting").
        let state = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &cli_flags,
            now(),
        )
        .unwrap();
        assert_eq!(state.permission_mode, Some("prompting".to_string()));
    }

    #[test]
    fn session_start_sets_remote_control_fresh_from_this_launchs_own_cli_flags() {
        let cli_flags = ClaudeCliFlags {
            remote_control: true,
            name: None,
            permission_mode: None,
            launch_args: None,
        };
        let state = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &cli_flags,
            now(),
        )
        .unwrap();
        assert!(state.remote_control);
    }

    #[test]
    fn session_start_never_preserves_remote_control_from_a_prior_launch() {
        // ⚠️ Unlike model/permission_mode, remote_control must NOT carry
        // forward - a stale true from a prior launch would be exactly as
        // wrong as a stale false, since no hook field confirms either way.
        let with_remote_control = ClaudeCliFlags {
            remote_control: true,
            name: None,
            permission_mode: None,
            launch_args: None,
        };
        let prior = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &with_remote_control,
            now(),
        )
        .unwrap();
        assert!(prior.remote_control);

        let restarted = apply_event(
            Some(prior),
            "SessionStart",
            &input("C:/x"),
            "overmind",
            2,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert!(
            !restarted.remote_control,
            "this launch's own (default, false) cli_flags must win, not the prior state"
        );
    }

    #[test]
    fn session_end_deletes_the_state_by_returning_none() {
        let existing = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        assert_eq!(
            apply_event(
                existing,
                "SessionEnd",
                &input("C:/x"),
                "overmind",
                1,
                &ClaudeCliFlags::default(),
                now()
            ),
            None
        );
    }

    // -- Bootstrap: PM finding, 2026-09-19 -------------------------------- //
    // Lanes already running when the user-level hooks were installed
    // (synapse; the PM, via --resume) had no state file, because every
    // event before this fix was silently ignored without a prior
    // SessionStart - blocking the first real use for exactly the lanes
    // already mid-session. ⚠️ THIS REPLACES the old "does nothing" property
    // these two cases used to assert - that was the bug, not a property to
    // keep.

    #[test]
    fn an_event_with_no_prior_state_now_bootstraps_a_fresh_state() {
        let s = apply_event(
            None,
            "Stop",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .expect("must bootstrap, not stay None");
        assert_eq!(s.role, "overmind");
        assert_eq!(s.session_id, "s-123");
        assert_eq!(s.pid, 1);
        assert_eq!(s.cwd, "C:/x");
        assert_eq!(s.model, None, "unknown - never invented, never guessed");
        assert_eq!(
            s.updated_by_event, "Stop",
            "the real event, not a placeholder"
        );
        assert!(
            !s.busy,
            "Stop's own effect still applies on top of the bootstrap"
        );
    }

    #[test]
    fn pre_tool_use_with_no_state_bootstraps_a_correct_state() {
        // PM's own wording: "a PreToolUse with no state creates a correct
        // state." permission_mode/remote_control/launch_args come from the
        // REAL launch's own command line, read via cli_flags - not invented,
        // not left at a stale prior value (there is none).
        let cli_flags = ClaudeCliFlags {
            remote_control: true,
            name: None,
            permission_mode: Some("bypassPermissions".to_string()),
            launch_args: Some(vec![
                "claude.exe".to_string(),
                "--remote-control".to_string(),
            ]),
        };
        let s = apply_event(
            None,
            "PreToolUse",
            &input_without_permission_mode("C:/Projects/OverMind"),
            "overmind",
            42,
            &cli_flags,
            now(),
        )
        .expect("must bootstrap, not stay None");
        assert_eq!(s.role, "overmind");
        assert_eq!(s.pid, 42);
        assert_eq!(s.cwd, "C:/Projects/OverMind");
        assert_eq!(s.session_id, "s-123");
        assert!(s.remote_control);
        assert_eq!(s.permission_mode, Some("bypassPermissions".to_string()));
        assert_eq!(s.launch_args, cli_flags.launch_args);
        assert!(s.busy, "PreToolUse's own effect still applies");
    }

    #[test]
    fn a_session_id_change_re_bootstraps_rather_than_merging_stale_counters() {
        // PM's own wording: "a session_id change re-bootstraps." A state
        // file for a DIFFERENT (old) session_id must never have its stale
        // busy/subagent counters carried into a genuinely new session -
        // treated the same as no state at all, not merged.
        let mut stale = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        stale.session_id = "old-session".to_string();
        stale.busy = true;
        stale.subagents_running = 3;

        let mut new_session_input = input("C:/x");
        new_session_input.session_id = "new-session".to_string();
        let s = apply_event(
            Some(stale),
            "Stop",
            &new_session_input,
            "overmind",
            2,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.session_id, "new-session");
        assert_eq!(s.pid, 2, "the NEW pid, not the stale session's own");
        assert_eq!(
            s.subagents_running, 0,
            "a stale session's counters must never survive into a new one"
        );
        assert!(
            !s.busy,
            "Stop's own effect, not the stale session's leftover busy=true"
        );
    }

    // -- the recorded cwd is the lane's HOME, not wherever it is now ------- //
    //
    // PM finding, 2026-10-02 (a real `--self` refusal): `SessionStart` also
    // fires on a compaction, mid-session, carrying the session's CURRENT
    // cwd. The PM's last compaction ran while it was in `C:\Projects\fuel`
    // and rewrote `pm.json`'s cwd to that, so the transcript lookup, the
    // relaunch directory and the liveness check all pointed at the wrong
    // place. A process's launch directory never changes, so the same pid
    // keeps what it already recorded.

    fn home_state(cwd: &str, pid: u32) -> LaneState {
        apply_event(
            None,
            "SessionStart",
            &input(cwd),
            "pm",
            pid,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap()
    }

    #[test]
    fn a_compaction_session_start_from_the_same_process_keeps_the_recorded_cwd() {
        let s = apply_event(
            Some(home_state("C:/Projects", 42)),
            "SessionStart",
            &input("C:/Projects/fuel"),
            "pm",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(
            s.cwd, "C:/Projects",
            "the same process's home, not where it has wandered"
        );
    }

    #[test]
    fn a_session_start_from_a_new_process_takes_its_own_cwd() {
        let s = apply_event(
            Some(home_state("C:/Projects", 42)),
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "pm",
            77,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, "C:/Projects/OverMind");
    }

    #[test]
    fn a_re_bootstrap_from_the_same_process_keeps_the_recorded_cwd_too() {
        let mut other_session = input("C:/Projects/fuel");
        other_session.session_id = "after-clear".to_string();
        let s = apply_event(
            Some(home_state("C:/Projects", 42)),
            "Stop",
            &other_session,
            "pm",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.session_id, "after-clear");
        assert_eq!(s.cwd, "C:/Projects");
    }

    /// The cross-check: the transcript's own directory names the launch
    /// directory (sessions.md's encoding), so a candidate that matches it
    /// beats one that doesn't - including a same-pid recorded cwd that was
    /// already wrong before this fix (the PM's own `pm.json`).
    #[test]
    fn the_cwd_matching_transcript_path_wins_over_a_wrong_recorded_one() {
        let mut compact = input("C:/Projects");
        compact.transcript_path =
            Some("C:/Users/u/.claude/projects/C--Projects/s-123.jsonl".to_string());
        let s = apply_event(
            Some(home_state("C:/Projects/fuel", 42)),
            "SessionStart",
            &compact,
            "pm",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, "C:/Projects");
    }

    /// A real Windows hook payload's `transcript_path` uses backslashes; the
    /// parent-directory read must not depend on the host's own separator.
    #[test]
    fn a_backslashed_transcript_path_is_read_the_same_way() {
        let mut compact = input(r"C:\Projects");
        compact.transcript_path =
            Some(r"C:\Users\u\.claude\projects\C--Projects\s-123.jsonl".to_string());
        let s = apply_event(
            Some(home_state(r"C:\Projects\fuel", 42)),
            "SessionStart",
            &compact,
            "pm",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, r"C:\Projects");
    }

    /// Fail closed: with no candidate matching the transcript, nothing is
    /// guessed - the same-process rule stands, and `decide`'s own transcript
    /// check refuses a restart rather than relaunching in the wrong place.
    #[test]
    fn with_no_candidate_matching_transcript_path_the_same_process_rule_stands() {
        let mut compact = input("C:/Projects/coderipper");
        compact.transcript_path =
            Some("C:/Users/u/.claude/projects/C--Projects/s-123.jsonl".to_string());
        let s = apply_event(
            Some(home_state("C:/Projects/fuel", 42)),
            "SessionStart",
            &compact,
            "pm",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, "C:/Projects/fuel");
    }

    // -- a cwd recorded wrong BEFORE the fix heals on a later event -------- //
    //
    // PM task, 2026-10-03 (Humboldt's real `--self` refusal): its session
    // compacted in `...\backend-foundation\frontend` under the old binary,
    // which recorded that subdirectory. The transcript cross-check ran only
    // on `SessionStart` and bootstrap, so every later event kept the wrong
    // cwd for the life of the process. Now any event from the SAME process
    // takes `recorded_cwd`'s answer, which moves the cwd only on proof.

    const HUMBOLDT_HOME: &str = r"C:\Projects\Humboldt\.claude\worktrees\backend-foundation";
    const HUMBOLDT_SUBDIR: &str =
        r"C:\Projects\Humboldt\.claude\worktrees\backend-foundation\frontend";
    const HUMBOLDT_TRANSCRIPT: &str = r"C:\Users\u\.claude\projects\C--Projects-Humboldt--claude-worktrees-backend-foundation\s-123.jsonl";

    fn later_event(cwd: &str, transcript_path: Option<&str>) -> HookInput {
        HookInput {
            transcript_path: transcript_path.map(str::to_string),
            ..input(cwd)
        }
    }

    #[test]
    fn a_later_event_with_transcript_proof_heals_a_wrongly_recorded_cwd() {
        let s = apply_event(
            Some(home_state(HUMBOLDT_SUBDIR, 42)),
            "PreToolUse",
            &later_event(HUMBOLDT_HOME, Some(HUMBOLDT_TRANSCRIPT)),
            "humboldt",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, HUMBOLDT_HOME);
    }

    /// Negative control: the same event without `transcript_path` proves
    /// nothing, so the recorded cwd stays - healing is never a guess.
    #[test]
    fn a_later_event_without_transcript_proof_leaves_the_cwd_unchanged() {
        let s = apply_event(
            Some(home_state(HUMBOLDT_SUBDIR, 42)),
            "PreToolUse",
            &later_event(HUMBOLDT_HOME, None),
            "humboldt",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, HUMBOLDT_SUBDIR);
    }

    /// Negative control: a transcript that matches neither candidate proves
    /// nothing either.
    #[test]
    fn a_later_event_whose_transcript_matches_neither_cwd_leaves_it_unchanged() {
        let s = apply_event(
            Some(home_state(HUMBOLDT_SUBDIR, 42)),
            "PreToolUse",
            &later_event(
                r"C:\Projects\Humboldt\.claude\worktrees\backend-foundation\backend",
                Some(HUMBOLDT_TRANSCRIPT),
            ),
            "humboldt",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, HUMBOLDT_SUBDIR);
    }

    /// The right cwd is never moved by a later event from a subdirectory,
    /// even when the event carries a transcript path.
    #[test]
    fn a_later_event_from_a_subdirectory_keeps_a_correctly_recorded_cwd() {
        let s = apply_event(
            Some(home_state(HUMBOLDT_HOME, 42)),
            "PreToolUse",
            &later_event(HUMBOLDT_SUBDIR, Some(HUMBOLDT_TRANSCRIPT)),
            "humboldt",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, HUMBOLDT_HOME);
    }

    /// Only the recorded process heals its own state: an event from another
    /// pid under the same session leaves the cwd alone, proof or not.
    #[test]
    fn a_later_event_from_another_process_never_moves_the_cwd() {
        let s = apply_event(
            Some(home_state(HUMBOLDT_SUBDIR, 42)),
            "PreToolUse",
            &later_event(HUMBOLDT_HOME, Some(HUMBOLDT_TRANSCRIPT)),
            "humboldt",
            77,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.cwd, HUMBOLDT_SUBDIR);
    }

    #[test]
    fn a_session_start_created_state_missing_launch_args_is_refreshed_on_a_later_event() {
        // PM's own case: "the PM's own pm.json has launch_args=None" - a
        // state a real SessionStart created before this fix (or with an
        // unreadable cmdline at the time) never recorded launch_args at all.
        // Refreshed opportunistically here, on ANY later event for the SAME
        // session, rather than left empty until the lane's next real
        // SessionStart.
        let mut existing_no_launch_args = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(existing_no_launch_args.launch_args, None);
        existing_no_launch_args.session_id = "s-123".to_string();

        let cli_flags = ClaudeCliFlags {
            launch_args: Some(vec![
                "claude.exe".to_string(),
                "--dangerously-load-development-channels".to_string(),
                "server:claude-peers".to_string(),
            ]),
            ..ClaudeCliFlags::default()
        };
        let s = apply_event(
            Some(existing_no_launch_args),
            "PreToolUse",
            &input("C:/x"),
            "overmind",
            1,
            &cli_flags,
            now(),
        )
        .unwrap();
        assert_eq!(s.launch_args, cli_flags.launch_args);
    }

    #[test]
    fn an_existing_state_with_launch_args_already_set_is_left_alone() {
        // The refresh must be additive (fills a gap), never overwrite a
        // real, already-recorded value with a DIFFERENT later launch's
        // command line - only None is ever replaced.
        let mut existing = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags {
                launch_args: Some(vec!["claude.exe".to_string(), "--original".to_string()]),
                ..ClaudeCliFlags::default()
            },
            now(),
        )
        .unwrap();
        existing.session_id = "s-123".to_string();

        let cli_flags = ClaudeCliFlags {
            launch_args: Some(vec!["claude.exe".to_string(), "--different".to_string()]),
            ..ClaudeCliFlags::default()
        };
        let s = apply_event(
            Some(existing),
            "PreToolUse",
            &input("C:/x"),
            "overmind",
            1,
            &cli_flags,
            now(),
        )
        .unwrap();
        assert_eq!(
            s.launch_args,
            Some(vec!["claude.exe".to_string(), "--original".to_string()]),
            "an already-recorded launch_args must never be overwritten"
        );
    }

    #[test]
    fn user_prompt_submit_and_pre_tool_use_both_set_busy() {
        for event in ["UserPromptSubmit", "PreToolUse"] {
            let s = apply_event(
                None,
                "SessionStart",
                &input("C:/x"),
                "overmind",
                1,
                &ClaudeCliFlags::default(),
                now(),
            );
            let s = apply_event(
                s,
                event,
                &input("C:/x"),
                "overmind",
                1,
                &ClaudeCliFlags::default(),
                now(),
            )
            .unwrap();
            assert!(s.busy, "{event} must set busy");
        }
    }

    #[test]
    fn stop_clears_busy() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "PreToolUse",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "Stop",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert!(!s.busy);
    }

    #[test]
    fn subagent_start_increments_and_stop_decrements() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "SubagentStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "SubagentStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.subagents_running, 2);
        let s = apply_event(
            Some(s),
            "SubagentStop",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.subagents_running, 1);
    }

    #[test]
    fn subagent_stop_never_goes_below_zero() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "SubagentStop",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.subagents_running, 0);
    }

    #[test]
    fn post_model_switch_updates_the_model() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let mut with_model = input("C:/x");
        with_model.model = Some(ModelField::Plain("claude-opus-5".to_string()));
        let s = apply_event(
            s,
            "PostModelSwitch",
            &with_model,
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.model, Some("claude-opus-5".to_string()));
    }

    #[test]
    fn post_model_switch_with_no_model_field_leaves_it_unchanged() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "PostModelSwitch",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.model, None);
    }

    #[test]
    fn no_background_shells_is_never_set_true_by_any_hook_event() {
        // RESTART-TOOL-DESIGN.md §1a: that claim is the lane's own manual
        // assertion when writing HANDOFF, never something a lifecycle
        // hook can honestly assert on the lane's behalf.
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        for event in [
            "UserPromptSubmit",
            "PreToolUse",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "PostModelSwitch",
        ] {
            let s2 = apply_event(
                s.clone(),
                event,
                &input("C:/x"),
                "overmind",
                1,
                &ClaudeCliFlags::default(),
                now(),
            )
            .unwrap();
            assert_ne!(s2.no_background_shells, Some(true));
        }
    }

    // -- AssertIdle ------------------------------------------------------- //
    // RESTART-TOOL-DESIGN.md §1a. PM finding, 2026-09-18: no production path
    // ever wrote `no_background_shells: Some(true)` before this event
    // existed - every real restart refused, and every existing test's
    // fixture hard-coded the field, so nothing caught it.

    #[test]
    fn assert_idle_sets_no_background_shells_true() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "AssertIdle",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.no_background_shells, Some(true));
    }

    #[test]
    fn a_later_user_prompt_submit_clears_a_stale_assert_idle() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "AssertIdle",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        assert_eq!(s.as_ref().unwrap().no_background_shells, Some(true));

        let s = apply_event(
            s,
            "UserPromptSubmit",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(
            s.no_background_shells, None,
            "a stale assertion must not survive the lane doing more work"
        );
    }

    #[test]
    fn a_later_pre_tool_use_clears_a_stale_assert_idle_too() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "AssertIdle",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        let s = apply_event(
            s,
            "PreToolUse",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(s.no_background_shells, None);
    }

    #[test]
    fn apply_event_bootstraps_even_for_a_direct_assert_idle_call() {
        // ⚠️ `apply_event` is a pure, generic function - it now bootstraps
        // uniformly for ANY event with no matching existing state,
        // `AssertIdle` included, when called directly. In PRODUCTION this
        // path is never reached for `AssertIdle`: `run_assert_idle`'s own
        // SEPARATE, STRICTER guard refuses before ever calling `apply_event`
        // at all - see `run_assert_idle_refuses_when_no_session_start_was_
        // ever_recorded`, unchanged by this fix, which is what actually
        // proves "existing behaviour is unchanged" at the real entry point.
        let s = apply_event(
            None,
            "AssertIdle",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .expect("apply_event itself now bootstraps for any event, AssertIdle included");
        assert_eq!(s.no_background_shells, Some(true));
    }

    #[test]
    fn run_assert_idle_refuses_when_no_session_start_was_ever_recorded() {
        let dir = tempdir().unwrap();
        let lookup = FakeAncestry::chain(&[(4242, 99, "claude.exe")]);
        let err = run_assert_idle(dir.path(), 4242, &lookup, Some("overmind"), "C:/x")
            .expect_err("assert-idle with no prior SessionStart must be refused, not silent");
        assert!(
            err.contains("no state file"),
            "refusal message must say why - got {err:?}"
        );
    }

    /// ⚠️ THE END-TO-END TEST THE PM ASKED FOR: hook JSON on stdin -> a real
    /// state file on disk -> `authorize::decide()` reading that SAME file -
    /// no hand-built `LaneState` fixture anywhere in this test, so
    /// "no production path ever sets this field" can't hide behind a
    /// fixture that assumes the field is already set, the way every other
    /// `decide()` test up to now has.
    #[test]
    fn a_real_assert_idle_run_produces_a_state_file_that_decide_actually_accepts() {
        use crate::authorize::{self, Request, Target};
        use crate::facts::SystemFacts;

        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let role = "overmind";
        let my_pid = 4242u32;

        // hook JSON -> state file (SessionStart), exactly as a real hook
        // invocation would produce it.
        let session_start_json =
            r#"{"hook_event_name":"SessionStart","session_id":"e2e-session","cwd":"C:/Projects/OverMind"}"#
                .to_string();
        let mut stdin = session_start_json.as_bytes();
        run(
            state_dir,
            "SessionStart",
            my_pid,
            &FakeAncestry::chain(&[(my_pid, 99, "claude.exe")]),
            Some(role),
            &mut stdin,
        )
        .unwrap();

        // The lane asserting idleness about ITSELF, the real command a
        // lane runs from its own shell - not a hook, no stdin JSON.
        run_assert_idle(
            state_dir,
            my_pid,
            &FakeAncestry::chain(&[(my_pid, 99, "claude.exe")]),
            Some(role),
            "C:/Projects/OverMind",
        )
        .unwrap();

        // authorize::decide() reads the SAME file this test never touched
        // directly.
        struct RealPidAlive;
        impl SystemFacts for RealPidAlive {
            fn is_alive_claude_process(&self, pid: u32) -> bool {
                // The state file's pid is claude_parent_pid's RESULT (the
                // fake claude.exe pid, 99), not this hook process's own
                // pid (my_pid, 4242) - run() derives them separately.
                pid == 99
            }
            fn cwd_of(&self, _pid: u32) -> Option<std::path::PathBuf> {
                Some(std::path::PathBuf::from("C:/Projects/OverMind"))
            }
            fn has_live_shell_descendant(
                &self,
                _pid: u32,
            ) -> Result<bool, crate::facts::ShellCheckError> {
                Ok(false)
            }
            fn transcript_is_recent(
                &self,
                _cwd: &str,
                session_id: &str,
                _max_age: std::time::Duration,
            ) -> bool {
                // Only the real session's transcript exists - an
                // assert-idle that loses the session_id must fail here,
                // exactly as it did live on 2026-09-27.
                session_id == "e2e-session"
            }
            fn now(&self) -> chrono::DateTime<Utc> {
                Utc::now()
            }
            fn process_identity(&self, pid: u32) -> Option<crate::facts::ProcessIdentity> {
                (pid == 99).then_some(crate::facts::ProcessIdentity {
                    start_time_secs: 0,
                    exe: None,
                })
            }
            fn kill_verified(
                &self,
                _pid: u32,
                _expected: &crate::facts::ProcessIdentity,
            ) -> Result<(), crate::facts::KillError> {
                unreachable!("this test never kills anything")
            }
            fn find_claude_process_in(
                &self,
                _cwd: &str,
                _after_start_time_secs: u64,
            ) -> Option<u32> {
                unreachable!("this test never relaunches anything")
            }
            fn process_table(
                &self,
            ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
                unreachable!("this test never relaunches anything")
            }
        }

        // Target::Other, not Myself: the no_background_shells check
        // (idle_and_shell_free) only runs for a DIFFERENT lane restarting
        // this one - RESTART-TOOL-DESIGN.md §3. Myself skips it entirely,
        // which would make this test pass for the wrong reason.
        let plan = authorize::decide(
            &Request {
                target: Target::Other {
                    role: role.to_string(),
                },
                confirmed: false,
                dry_run: true,
            },
            &RealPidAlive,
            state_dir,
        )
        .expect(
            "a state file written by run() then run_assert_idle() must be accepted by decide()",
        );
        assert_eq!(plan.state.no_background_shells, Some(true));
        assert_eq!(plan.state.session_id, "e2e-session");
    }

    /// The PM's own 2026-10-02 refusal, end to end: launched in
    /// `C:\Projects`, compacted while in `fuel`, process now sitting in
    /// `coderipper`. Real hook JSON in, the real state file, `decide()` on
    /// it, and the caller's pid from the same ancestry walk `main` uses.
    #[test]
    fn a_lane_that_compacted_elsewhere_can_still_restart_itself_and_only_itself() {
        use crate::authorize::{self, Refusal, Request, Target};
        use crate::facts::SystemFacts;

        let dir = tempdir().unwrap();
        let state_dir = dir.path();
        let hook_pid = 4242u32;
        let pm_ancestry = FakeAncestry::chain(&[(hook_pid, 99, "claude.exe")]);
        let transcript = r#"C:\\Users\\u\\.claude\\projects\\C--Projects\\pm-session.jsonl"#;
        for (event, cwd) in [
            ("SessionStart", r#"C:\\Projects"#),
            ("SessionStart", r#"C:\\Projects\\fuel"#), // the compaction
            ("Stop", r#"C:\\Projects\\fuel"#),
        ] {
            let json = format!(
                r#"{{"hook_event_name":"{event}","session_id":"pm-session","cwd":"{cwd}","transcript_path":"{transcript}"}}"#
            );
            run(
                state_dir,
                event,
                hook_pid,
                &pm_ancestry,
                Some("pm"),
                &mut json.as_bytes(),
            )
            .unwrap();
        }

        struct Wandered;
        impl SystemFacts for Wandered {
            fn is_alive_claude_process(&self, pid: u32) -> bool {
                pid == 99
            }
            fn cwd_of(&self, _pid: u32) -> Option<std::path::PathBuf> {
                Some(std::path::PathBuf::from(r"C:\Projects\coderipper\"))
            }
            fn has_live_shell_descendant(
                &self,
                _pid: u32,
            ) -> Result<bool, crate::facts::ShellCheckError> {
                Ok(false)
            }
            fn transcript_is_recent(
                &self,
                cwd: &str,
                session_id: &str,
                _max_age: std::time::Duration,
            ) -> bool {
                // The transcript lives under the LAUNCH directory only.
                crate::paths::paths_match(cwd, r"C:\Projects") && session_id == "pm-session"
            }
            fn now(&self) -> chrono::DateTime<Utc> {
                Utc::now()
            }
            fn process_identity(&self, pid: u32) -> Option<crate::facts::ProcessIdentity> {
                (pid == 99).then_some(crate::facts::ProcessIdentity {
                    start_time_secs: 0,
                    exe: None,
                })
            }
            fn kill_verified(
                &self,
                _pid: u32,
                _expected: &crate::facts::ProcessIdentity,
            ) -> Result<(), crate::facts::KillError> {
                unreachable!("this test never kills anything")
            }
            fn find_claude_process_in(
                &self,
                _cwd: &str,
                _after_start_time_secs: u64,
            ) -> Option<u32> {
                unreachable!("this test never relaunches anything")
            }
            fn process_table(
                &self,
            ) -> Result<Vec<crate::facts::ProcEntry>, crate::facts::ShellCheckError> {
                unreachable!("this test never relaunches anything")
            }
        }

        let self_request = |caller_ancestry: &FakeAncestry, caller: u32| Request {
            target: Target::Myself {
                role: "pm".to_string(),
                caller_pid: claude_parent_pid(caller, caller_ancestry).ok(),
            },
            confirmed: false,
            dry_run: true,
        };

        let plan = authorize::decide(&self_request(&pm_ancestry, hook_pid), &Wandered, state_dir)
            .expect("the PM's own session must be able to restart itself");
        assert_eq!(
            plan.state.cwd, r"C:\Projects",
            "the relaunch directory is the launch directory, not fuel"
        );

        // Another lane's session, naming the PM's role with --self.
        let other_lane = FakeAncestry::chain(&[(5555, 77, "claude.exe")]);
        assert!(matches!(
            authorize::decide(&self_request(&other_lane, 5555), &Wandered, state_dir),
            Err(Refusal::NotTheCaller { .. })
        ));
    }

    // -- StateLock: the concurrency-safety property itself ------------------ //

    #[test]
    fn a_second_acquire_blocks_until_the_first_is_dropped() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("overmind.lock");
        let first = StateLock::acquire(
            lock_path.clone(),
            Duration::from_millis(200),
            Duration::from_secs(10),
        )
        .unwrap();

        let path_for_thread = lock_path.clone();
        let handle = std::thread::spawn(move || {
            StateLock::acquire(
                path_for_thread,
                Duration::from_secs(2),
                Duration::from_secs(10),
            )
        });

        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        assert!(handle.join().unwrap().is_ok());
    }

    #[test]
    fn acquire_times_out_rather_than_blocking_forever() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("overmind.lock");
        let _held = StateLock::acquire(
            lock_path.clone(),
            Duration::from_millis(200),
            Duration::from_secs(10),
        )
        .unwrap();
        let result = StateLock::acquire(
            lock_path,
            Duration::from_millis(100),
            Duration::from_secs(10),
        );
        assert!(matches!(result, Err(WriteError::LockTimedOut)));
    }

    #[test]
    fn a_stale_lock_is_reclaimed_rather_than_wedging_forever() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("overmind.lock");
        std::fs::write(&lock_path, "").unwrap();
        // Simulate an old lock by backdating its mtime.
        let old = std::time::SystemTime::now() - Duration::from_secs(30);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap();
        file.set_modified(old).unwrap();

        let result = StateLock::acquire(
            lock_path,
            Duration::from_secs(2),
            Duration::from_millis(500),
        );
        assert!(
            result.is_ok(),
            "a lock older than stale_after must be reclaimed"
        );
    }

    // -- LOCK_ACQUIRE_TIMEOUT / LOCK_STALE_AFTER -------------------------- //
    // ⚠️ PM finding, 2026-09-19 (install, hook-errors.log: two real "timed
    // out waiting for the lane-state lock" errors): the production call
    // sites had timeout (2s) SHORTER than stale_after (5s) - every waiter
    // gave up before anyone was even ALLOWED to reclaim an abandoned lock,
    // dropping the event. A lost SubagentStop leaves the count HIGH (the
    // safe direction) but can block a legitimate restart until the next
    // SessionStart.

    #[test]
    fn the_production_timeout_exceeds_the_production_stale_after() {
        // ⚠️ THE EXACT PROPERTY THE BUG VIOLATED. A waiter must be able to
        // survive long enough to actually perform the reclaim.
        assert!(
            LOCK_ACQUIRE_TIMEOUT > LOCK_STALE_AFTER,
            "timeout ({LOCK_ACQUIRE_TIMEOUT:?}) must exceed stale_after \
             ({LOCK_STALE_AFTER:?}), or every waiter gives up before an \
             abandoned lock becomes reclaimable"
        );
    }

    #[test]
    fn a_waiter_reclaims_a_stale_lock_using_the_real_production_constants() {
        // The bug, reproduced with the REAL constants (not a synthetic
        // timeout/stale pair chosen to make the test pass): a lock backdated
        // past LOCK_STALE_AFTER but well within LOCK_ACQUIRE_TIMEOUT must be
        // reclaimed, not time out first.
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("overmind.lock");
        std::fs::write(&lock_path, "").unwrap();
        let old = std::time::SystemTime::now() - LOCK_STALE_AFTER - Duration::from_secs(1);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap();
        file.set_modified(old).unwrap();

        let result = StateLock::acquire(lock_path, LOCK_ACQUIRE_TIMEOUT, LOCK_STALE_AFTER);
        assert!(
            result.is_ok(),
            "with the real production constants, a stale lock must be \
             reclaimed well before LOCK_ACQUIRE_TIMEOUT elapses"
        );
    }

    #[test]
    fn concurrent_subagent_start_events_are_never_lost() {
        // ⚠️ THE EXACT RACE THE PM FOUND: an unguarded read-modify-write on
        // one JSON file loses updates under concurrency, undercounting
        // subagents_running - the unsafe direction. This drives real OS
        // threads at the real file-write path (not just apply_event in
        // isolation) to prove the lock actually serialises them.
        let dir = tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let role = "overmind";
        let path = state_dir.join(format!("{role}.json"));

        let initial = apply_event(
            None,
            "SessionStart",
            &input("C:/Projects/OverMind"),
            role,
            1,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        write_atomic(&path, &initial).unwrap();

        let n = 20;
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let state_dir = state_dir.clone();
                std::thread::spawn(move || {
                    let lock = StateLock::acquire(
                        state_dir.join(format!("{role}.lock")),
                        Duration::from_secs(5),
                        Duration::from_secs(10),
                    )
                    .unwrap();
                    let existing = crate::state::load(&state_dir, role).ok();
                    let next = apply_event(
                        existing,
                        "SubagentStart",
                        &input("C:/Projects/OverMind"),
                        role,
                        1,
                        &ClaudeCliFlags::default(),
                        now(),
                    )
                    .unwrap();
                    write_atomic(&state_dir.join(format!("{role}.json")), &next).unwrap();
                    drop(lock);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_state = crate::state::load(&state_dir, role).unwrap();
        assert_eq!(
            final_state.subagents_running, n as u32,
            "every concurrent SubagentStart must be counted, none lost"
        );
    }

    // -- RealParentProcess::cmdline_of ----------------------------------- //
    // PM finding, 2026-09-18 (first real restart attempt): a real session
    // launched with `--remote-control` still recorded `remote_control:
    // false`, because plain `refresh_processes` never populates `cmd` -
    // `FakeAncestry`'s stub `cmdline_of` (always `None`) couldn't see that
    // gap. This spawns a REAL child with a KNOWN argv and reads it back
    // through `RealParentProcess` itself.

    #[test]
    fn real_parent_process_cmdline_of_reads_a_real_spawned_childs_actual_argv() {
        #[cfg(windows)]
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("could not spawn a throwaway child process for this test");
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("could not spawn a throwaway child process for this test");
        let pid = child.id();

        std::thread::sleep(std::time::Duration::from_millis(200));
        let lookup = RealParentProcess;
        let observed = lookup
            .cmdline_of(pid)
            .expect("a real spawned child's argv must be readable, not None");

        let _ = child.kill();
        let _ = child.wait();

        assert!(
            observed.iter().any(|a| a == "-n" || a == "30"),
            "cmdline_of must read the child's REAL argv, not an empty one - got {observed:?}"
        );
    }

    // -- which file an event belongs to: the launch dir, not the event cwd -- //
    //
    // PM task, 2026-10-04 (agentlife DESIGN-REVISION-1 §2.2.1): the file's
    // role came from the EVENT's cwd leaf, so a lane without LANE_ROLE that
    // cd'd into a subdirectory or a sibling worktree wrote `<that dir>.json`
    // (43 state files for 14 live lanes, measured 2026-10-03). #105/#107
    // fixed the recorded cwd FIELD, not the file NAME.

    const LANE: &str = "C:/p/lane";
    const LANE_TRANSCRIPT: &str = r"C:\u\.claude\projects\C--p-lane\s1.jsonl";

    fn hook_json(event: &str, session: &str, cwd: &str, transcript: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "hook_event_name": event, "session_id": session, "cwd": cwd,
        });
        if let Some(t) = transcript {
            v["transcript_path"] = serde_json::json!(t);
        }
        v.to_string()
    }

    fn run_event(state_dir: &Path, event: &str, json: &str, role_env: Option<&str>) {
        let mut stdin = json.as_bytes();
        run(
            state_dir,
            event,
            4242,
            &FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
            role_env,
            &mut stdin,
        )
        .unwrap();
    }

    fn state_files(state_dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(state_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_event_from_a_subdirectory_writes_the_launch_dirs_file() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);
        let sub = hook_json(
            "PreToolUse",
            "s1",
            "C:/p/lane/crates/sub",
            Some(LANE_TRANSCRIPT),
        );
        run_event(dir.path(), "PreToolUse", &sub, None);

        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
        let s = crate::state::load(dir.path(), "lane").unwrap();
        assert_eq!(s.updated_by_event, "PreToolUse");
        assert_eq!(s.cwd, LANE);
    }

    /// With no state file yet (the first event after hooks are installed
    /// mid-session), only the transcript-proven ancestor names the file.
    #[test]
    fn a_first_event_from_a_subdirectory_still_names_the_launch_dirs_file() {
        let dir = tempdir().unwrap();
        let sub = hook_json(
            "PreToolUse",
            "s1",
            "C:/p/lane/crates/sub",
            Some(LANE_TRANSCRIPT),
        );
        run_event(dir.path(), "PreToolUse", &sub, None);
        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
    }

    /// The file NAME is what a write targets; a `role` field that says
    /// otherwise (a hand edit, an old writer) must not send events to a
    /// third file (review of #0).
    #[test]
    fn the_file_name_wins_over_the_role_field_inside_it() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);
        let path = dir.path().join("lane.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["role"] = serde_json::json!("other");
        std::fs::write(&path, v.to_string()).unwrap();
        let sibling = hook_json("PreToolUse", "s1", "C:/p/lane-wt", Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "PreToolUse", &sibling, None);
        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
    }

    #[test]
    fn an_event_from_a_sibling_worktree_writes_the_launch_dirs_file() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);
        let sibling = hook_json("PreToolUse", "s1", "C:/p/lane-wt", Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "PreToolUse", &sibling, None);

        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
        assert_eq!(
            crate::state::load(dir.path(), "lane")
                .unwrap()
                .updated_by_event,
            "PreToolUse"
        );
    }

    /// Negative control: with no transcript path nothing is proven, so the
    /// event's own cwd decides, as before.
    #[test]
    fn without_a_transcript_path_the_event_cwd_still_decides() {
        let dir = tempdir().unwrap();
        let sub = hook_json("PreToolUse", "s1", "C:/p/lane/sub", None);
        run_event(dir.path(), "PreToolUse", &sub, None);
        assert_eq!(state_files(dir.path()), vec!["sub.json"]);
    }

    /// Negative control: a sibling directory with no state file of this
    /// session keeps today's behaviour.
    #[test]
    fn a_sibling_with_no_file_of_this_session_keeps_todays_behaviour() {
        let dir = tempdir().unwrap();
        let other = hook_json("SessionStart", "other", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &other, None);
        let sibling = hook_json("PreToolUse", "s1", "C:/p/lane-wt", Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "PreToolUse", &sibling, None);
        assert_eq!(state_files(dir.path()), vec!["lane-wt.json", "lane.json"]);
    }

    #[test]
    fn lane_role_still_wins_over_the_launch_dir() {
        let dir = tempdir().unwrap();
        let sub = hook_json("PreToolUse", "s1", "C:/p/lane/sub", Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "PreToolUse", &sub, Some("pm"));
        assert_eq!(state_files(dir.path()), vec!["pm.json"]);
    }

    #[test]
    fn assert_idle_from_a_sibling_worktree_finds_its_own_file() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);

        run_assert_idle(
            dir.path(),
            4242,
            &FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
            None,
            "C:/p/lane-wt",
        )
        .unwrap();

        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
        let s = crate::state::load(dir.path(), "lane").unwrap();
        assert_eq!(s.no_background_shells, Some(true));
    }

    /// Negative control: no state file for the caller's own claude process
    /// keeps today's refusal, named after the cwd leaf.
    #[test]
    fn assert_idle_with_no_file_for_its_process_still_refuses() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);

        let err = run_assert_idle(
            dir.path(),
            5555,
            &FakeAncestry::chain(&[(5555, 77, "claude.exe")]),
            None,
            "C:/p/lane-wt",
        )
        .unwrap_err();
        assert!(err.starts_with("lane-wt: no state file"), "{err}");
        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
    }

    // -- `name`, from the launch command line ------------------------------ //

    #[test]
    fn the_name_is_read_from_every_form_of_the_flag() {
        for (argv, want) in [
            (vec!["claude", "--name", "pm"], Some("pm")),
            (vec!["claude", "--name=pm"], Some("pm")),
            (vec!["claude", "-n", "PM"], Some("PM")),
            (vec!["claude", "--model", "x"], None),
        ] {
            assert_eq!(
                parse_claude_cli_flags(&strs(&argv)).name.as_deref(),
                want,
                "{argv:?}"
            );
        }
    }

    fn launched(argv: &[&str]) -> ClaudeCliFlags {
        parse_claude_cli_flags(&strs(argv))
    }

    #[test]
    fn session_start_records_the_launch_name() {
        let s = apply_event(
            None,
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &launched(&["claude", "-n", "auth-framework-deps"]),
            now(),
        )
        .unwrap();
        assert_eq!(s.name.as_deref(), Some("auth-framework-deps"));
    }

    /// A launch whose command line was read and has no name is unnamed, the
    /// same "always fresh" rule as `remote_control`; an unreadable one
    /// keeps what was known.
    #[test]
    fn a_fresh_launch_without_a_name_is_unnamed_but_an_unreadable_one_keeps_it() {
        let named = apply_event(
            None,
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &launched(&["claude", "--name", "old"]),
            now(),
        )
        .unwrap();
        let fresh = apply_event(
            Some(named.clone()),
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            77,
            &launched(&["claude"]),
            now(),
        )
        .unwrap();
        assert_eq!(fresh.name, None);
        let unreadable = apply_event(
            Some(named),
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &ClaudeCliFlags::default(),
            now(),
        )
        .unwrap();
        assert_eq!(unreadable.name.as_deref(), Some("old"));
    }

    #[test]
    fn a_later_event_fills_in_a_name_a_state_never_had() {
        let unnamed = home_state("C:/Projects", 42);
        assert_eq!(unnamed.name, None);
        let s = apply_event(
            Some(unnamed),
            "PreToolUse",
            &input("C:/Projects"),
            "pm",
            42,
            &launched(&["claude", "--name", "pm"]),
            now(),
        )
        .unwrap();
        assert_eq!(s.name.as_deref(), Some("pm"));
    }

    // -- the claude process's start time, beside its pid -------------------- //

    struct FakeWithStart(FakeAncestry, Option<u64>);
    impl ParentProcess for FakeWithStart {
        fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
            self.0.parent_of(pid)
        }
        fn cmdline_of(&self, pid: u32) -> Option<Vec<String>> {
            self.0.cmdline_of(pid)
        }
        fn start_time_of(&self, pid: u32) -> Option<u64> {
            (pid == 99).then_some(self.1?)
        }
    }

    #[test]
    fn the_claude_process_start_time_is_recorded_beside_its_pid() {
        let dir = tempdir().unwrap();
        let json = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        let mut stdin = json.as_bytes();
        run(
            dir.path(),
            "SessionStart",
            4242,
            &FakeWithStart(
                FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
                Some(1_700_000_000),
            ),
            None,
            &mut stdin,
        )
        .unwrap();
        let s = crate::state::load(dir.path(), "lane").unwrap();
        assert_eq!((s.pid, s.pid_start_secs), (99, Some(1_700_000_000)));
    }

    /// A same-session state can still carry an older pid (`apply_event`
    /// keeps it); the start time read for THIS hook's verified pid must not
    /// be written beside that other pid.
    #[test]
    fn a_start_time_is_never_written_beside_a_different_pid() {
        let dir = tempdir().unwrap();
        let mut stdin = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT)).into_bytes();
        run(
            dir.path(),
            "SessionStart",
            4242,
            &FakeAncestry::chain(&[(4242, 50, "claude.exe")]),
            None,
            &mut stdin.as_slice(),
        )
        .unwrap();
        stdin = hook_json("PreToolUse", "s1", LANE, Some(LANE_TRANSCRIPT)).into_bytes();
        run(
            dir.path(),
            "PreToolUse",
            4242,
            &FakeWithStart(
                FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
                Some(1_700_000_000),
            ),
            None,
            &mut stdin.as_slice(),
        )
        .unwrap();
        let s = crate::state::load(dir.path(), "lane").unwrap();
        assert_eq!((s.pid, s.pid_start_secs), (50, None));
    }

    /// State files written before this field existed still load.
    #[test]
    fn a_state_file_without_a_start_time_still_loads() {
        let dir = tempdir().unwrap();
        let json = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &json, None);
        let path = dir.path().join("lane.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("pid_start_secs");
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(
            crate::state::load(dir.path(), "lane")
                .unwrap()
                .pid_start_secs,
            None
        );
    }

    #[test]
    fn real_parent_process_start_time_of_reads_a_real_spawned_child() {
        #[cfg(windows)]
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("could not spawn a throwaway child process for this test");
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("could not spawn a throwaway child process for this test");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let started = RealParentProcess.start_time_of(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let started = started.expect("a real child's start time must be readable");
        assert!(
            started <= now_secs && now_secs - started < 120,
            "{started} vs now {now_secs}"
        );
    }

    // -- review of #0 (2026-10-04): the session's own file, by session + pid -- //

    fn run_as(state_dir: &Path, event: &str, json: &str, claude: u32, role_env: Option<&str>) {
        let mut stdin = json.as_bytes();
        run(
            state_dir,
            event,
            4242,
            &FakeAncestry::chain(&[(4242, claude, "claude.exe")]),
            role_env,
            &mut stdin,
        )
        .unwrap();
    }

    /// Claude Code moves the transcript into the worktree's project dir on
    /// `EnterWorktree`, so the transcript no longer names the launch dir.
    #[test]
    fn after_enter_worktree_events_still_go_to_the_launch_dirs_file() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);
        let moved = r"C:\u\.claude\projects\C--p-lane--claude-worktrees-wt\s1.jsonl";
        let ev = hook_json(
            "PreToolUse",
            "s1",
            "C:/p/lane/.claude/worktrees/wt/src",
            Some(moved),
        );
        run_event(dir.path(), "PreToolUse", &ev, None);
        assert_eq!(state_files(dir.path()), vec!["lane.json"]);
    }

    /// Files the old naming left carry the same session and pid; the launch
    /// dir's (the shortest recorded cwd) wins, not the first in dir order.
    #[test]
    fn of_two_files_of_this_process_the_launch_dirs_wins() {
        let dir = tempdir().unwrap();
        let wt = "C:/p/lane/.claude/worktrees/a-wt";
        run_event(
            dir.path(),
            "SessionStart",
            &hook_json("SessionStart", "s1", wt, None),
            Some("a-wt"),
        );
        run_event(
            dir.path(),
            "SessionStart",
            &hook_json("SessionStart", "s1", LANE, None),
            Some("lane"),
        );
        let ev = hook_json("PreToolUse", "s1", wt, None);
        run_event(dir.path(), "PreToolUse", &ev, None);
        let load = |r| crate::state::load(dir.path(), r).unwrap().updated_by_event;
        assert_eq!(
            (load("lane"), load("a-wt")),
            ("PreToolUse".to_string(), "SessionStart".to_string())
        );
    }

    /// A file of this session written by ANOTHER (dead) process is not this
    /// process's file, even when the transcript proves its cwd.
    #[test]
    fn a_file_of_this_session_from_another_process_is_not_followed() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_as(dir.path(), "SessionStart", &start, 10, Some("om"));
        let resumed = hook_json("SessionStart", "s1", "C:/p/overmind", None);
        run_as(dir.path(), "SessionStart", &resumed, 20, None);
        let ev = hook_json(
            "PreToolUse",
            "s1",
            "C:/p/overmind-wt",
            Some(LANE_TRANSCRIPT),
        );
        run_as(dir.path(), "PreToolUse", &ev, 20, None);
        let load = |r| crate::state::load(dir.path(), r).unwrap().updated_by_event;
        assert_eq!(load("overmind"), "PreToolUse");
        assert_eq!(load("om"), "SessionStart");
    }

    /// The cwd-leaf fallback can name ANOTHER lane's file; assert-idle must
    /// not mark that lane idle.
    #[test]
    fn assert_idle_never_marks_another_processs_file() {
        let dir = tempdir().unwrap();
        let b = hook_json("SessionStart", "sb", "C:/p/b", None);
        run_as(dir.path(), "SessionStart", &b, 50, None);
        let err = run_assert_idle(
            dir.path(),
            4242,
            &FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
            None,
            "C:/p/b",
        )
        .unwrap_err();
        assert!(err.contains("not this lane's claude pid 99"), "{err}");
        assert_eq!(
            crate::state::load(dir.path(), "b")
                .unwrap()
                .no_background_shells,
            None
        );
    }

    /// "Newest", not "first in directory order": the older file sorts first.
    #[test]
    fn assert_idle_takes_the_newest_file_even_when_an_older_one_sorts_first() {
        let dir = tempdir().unwrap();
        run_event(
            dir.path(),
            "SessionStart",
            &hook_json("SessionStart", "s1", "C:/p/a-old", None),
            None,
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        run_event(
            dir.path(),
            "SessionStart",
            &hook_json("SessionStart", "s2", LANE, None),
            None,
        );
        run_assert_idle(
            dir.path(),
            4242,
            &FakeAncestry::chain(&[(4242, 99, "claude.exe")]),
            None,
            "C:/p/lane-wt",
        )
        .unwrap();
        let shells = |r| {
            crate::state::load(dir.path(), r)
                .unwrap()
                .no_background_shells
        };
        assert_eq!((shells("lane"), shells("a-old")), (Some(true), None));
    }

    /// An empty command line (an access-denied read) is an unreadable one:
    /// it must not clear a known name.
    #[test]
    fn an_empty_command_line_keeps_the_known_name() {
        let named = apply_event(
            None,
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &launched(&["claude", "--name", "old"]),
            now(),
        )
        .unwrap();
        let s = apply_event(
            Some(named),
            "SessionStart",
            &input("C:/Projects/OverMind"),
            "overmind",
            42,
            &parse_claude_cli_flags(&[]),
            now(),
        )
        .unwrap();
        assert_eq!(s.name.as_deref(), Some("old"));
    }

    #[test]
    fn a_start_time_missed_at_session_start_is_filled_in_later() {
        let dir = tempdir().unwrap();
        let start = hook_json("SessionStart", "s1", LANE, Some(LANE_TRANSCRIPT));
        run_event(dir.path(), "SessionStart", &start, None);
        assert_eq!(
            crate::state::load(dir.path(), "lane")
                .unwrap()
                .pid_start_secs,
            None
        );
        let ev = hook_json("PreToolUse", "s1", LANE, Some(LANE_TRANSCRIPT));
        let mut stdin = ev.as_bytes();
        run(
            dir.path(),
            "PreToolUse",
            4242,
            &FakeWithStart(FakeAncestry::chain(&[(4242, 99, "claude.exe")]), Some(7)),
            None,
            &mut stdin,
        )
        .unwrap();
        assert_eq!(
            crate::state::load(dir.path(), "lane")
                .unwrap()
                .pid_start_secs,
            Some(7)
        );
    }
}
