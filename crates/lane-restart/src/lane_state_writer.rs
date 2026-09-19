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
    pub permission_mode: Option<String>,
}

/// Pure - given a command line, no I/O. `--dangerously-skip-permissions`
/// maps to Claude Code's own name for that mode (`bypassPermissions`,
/// confirmed in `sessions.md`'s permission-mode table), not a guessed
/// string. Accepts `--permission-mode value` and `--permission-mode=value`
/// both (PM finding, 2026-09-18, post-merge gate read: the equals form
/// wasn't parsed).
pub fn parse_claude_cli_flags(cmdline: &[String]) -> ClaudeCliFlags {
    let mut flags = ClaudeCliFlags::default();
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        if let Some(v) = arg.strip_prefix("--permission-mode=") {
            flags.permission_mode = Some(v.to_string());
            continue;
        }
        match arg.as_str() {
            "--remote-control" => flags.remote_control = true,
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
            cwd: input.cwd.clone(),
            name: existing.as_ref().and_then(|s| s.name.clone()),
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

    let mut state = existing?;
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

fn write_atomic(path: &Path, state: &LaneState) -> Result<(), WriteError> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state).map_err(|e| WriteError::Io(e.to_string()))?;
    std::fs::write(&tmp, json).map_err(|e| WriteError::Io(e.to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| WriteError::Io(e.to_string()))?;
    Ok(())
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

    let role = resolve_role(&input.cwd, lane_role_env);
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let path = state_dir.join(format!("{role}.json"));
    let lock = StateLock::acquire(
        state_dir.join(format!("{role}.lock")),
        Duration::from_secs(2),
        Duration::from_secs(5),
    )
    .map_err(|e| e.to_string())?;

    let existing = crate::state::load(state_dir, &role).ok();
    let pid = claude_parent_pid(my_pid, parent_lookup).map_err(|e| e.to_string())?;
    // ⚠️ Only ever read against the pid `claude_parent_pid` already
    // verified - never an unvetted process. Missing/unreadable cmdline is
    // "nothing detected", not a hard failure of the whole hook.
    let cli_flags = parent_lookup
        .cmdline_of(pid)
        .map(|cmd| parse_claude_cli_flags(&cmd))
        .unwrap_or_default();
    let next = apply_event(existing, event, &input, &role, pid, &cli_flags, Utc::now());

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
    let role = resolve_role(cwd, lane_role_env);
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let path = state_dir.join(format!("{role}.json"));
    let lock = StateLock::acquire(
        state_dir.join(format!("{role}.lock")),
        Duration::from_secs(2),
        Duration::from_secs(5),
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
    let cli_flags = parent_lookup
        .cmdline_of(pid)
        .map(|cmd| parse_claude_cli_flags(&cmd))
        .unwrap_or_default();
    // `AssertIdle`'s own `apply_event` branch reads only `existing` and
    // `cli_flags` - this placeholder carries no real session_id/model, both
    // of which only the `SessionStart` branch (never reached here) uses.
    let placeholder_input = HookInput {
        hook_event_name: "AssertIdle".to_string(),
        session_id: String::new(),
        cwd: cwd.to_string(),
        permission_mode: None,
        model: None,
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
    let result = match next {
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
                permission_mode: None,
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
                permission_mode: Some("prompting".to_string()),
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
        let cmdline = strs(&["claude.exe", "--name", "overmind"]);
        assert_eq!(parse_claude_cli_flags(&cmdline), ClaudeCliFlags::default());
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
                permission_mode: Some("bypassPermissions".to_string()),
            }
        );
    }

    #[test]
    fn a_trailing_permission_mode_flag_with_no_value_is_ignored_not_a_panic() {
        let cmdline = strs(&["claude.exe", "--permission-mode"]);
        assert_eq!(parse_claude_cli_flags(&cmdline), ClaudeCliFlags::default());
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
            permission_mode: Some("bypassPermissions".to_string()),
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
            permission_mode: Some("bypassPermissions".to_string()),
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
            permission_mode: None,
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
            permission_mode: None,
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

    #[test]
    fn an_event_with_no_prior_session_start_does_nothing() {
        assert_eq!(
            apply_event(
                None,
                "Stop",
                &input("C:/x"),
                "overmind",
                1,
                &ClaudeCliFlags::default(),
                now()
            ),
            None
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
    fn assert_idle_with_no_prior_session_start_does_nothing() {
        let s = apply_event(
            None,
            "AssertIdle",
            &input("C:/x"),
            "overmind",
            1,
            &ClaudeCliFlags::default(),
            now(),
        );
        assert_eq!(s, None);
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
                _session_id: &str,
                _max_age: std::time::Duration,
            ) -> bool {
                true
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
}
