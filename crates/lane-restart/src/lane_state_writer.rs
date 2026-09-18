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

/// The subset of hooks.md's documented common input fields this command
/// reads. Unknown fields are ignored by `serde_json`'s default behaviour -
/// no `deny_unknown_fields` here, since a future Claude Code version adding
/// fields must not break this parse.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub cwd: String,
    pub permission_mode: String,
    /// PM finding, 2026-09-18: not in the documented common-fields table for
    /// `PostModelSwitch` specifically, so this is read defensively as
    /// optional rather than assumed to exist under this exact name.
    pub model: Option<String>,
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
    now: chrono::DateTime<Utc>,
) -> Option<LaneState> {
    if event == "SessionStart" {
        return Some(LaneState {
            role: role.to_string(),
            session_id: input.session_id.clone(),
            pid,
            cwd: input.cwd.clone(),
            name: existing.as_ref().and_then(|s| s.name.clone()),
            model: existing.as_ref().and_then(|s| s.model.clone()),
            permission_mode: input.permission_mode.clone(),
            remote_control: existing.as_ref().map(|s| s.remote_control).unwrap_or(false),
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
        "UserPromptSubmit" | "PreToolUse" => state.busy = true,
        "Stop" => state.busy = false,
        "SubagentStart" => state.subagents_running += 1,
        "SubagentStop" => state.subagents_running = state.subagents_running.saturating_sub(1),
        "PostModelSwitch" => {
            if let Some(m) = &input.model {
                state.model = Some(m.clone());
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
    let next = apply_event(existing, event, &input, &role, pid, Utc::now());

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn input(cwd: &str) -> HookInput {
        HookInput {
            session_id: "s-123".to_string(),
            cwd: cwd.to_string(),
            permission_mode: "prompting".to_string(),
            model: None,
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
    fn session_start_preserves_a_previously_learned_model_across_a_resume() {
        let prior =
            apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now()).unwrap();
        let mut prior = prior;
        prior.model = Some("claude-opus-5".to_string());
        let restarted = apply_event(
            Some(prior),
            "SessionStart",
            &input("C:/x"),
            "overmind",
            2,
            now(),
        )
        .unwrap();
        assert_eq!(restarted.model, Some("claude-opus-5".to_string()));
    }

    #[test]
    fn session_end_deletes_the_state_by_returning_none() {
        let existing = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        assert_eq!(
            apply_event(existing, "SessionEnd", &input("C:/x"), "overmind", 1, now()),
            None
        );
    }

    #[test]
    fn an_event_with_no_prior_session_start_does_nothing() {
        assert_eq!(
            apply_event(None, "Stop", &input("C:/x"), "overmind", 1, now()),
            None
        );
    }

    #[test]
    fn user_prompt_submit_and_pre_tool_use_both_set_busy() {
        for event in ["UserPromptSubmit", "PreToolUse"] {
            let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
            let s = apply_event(s, event, &input("C:/x"), "overmind", 1, now()).unwrap();
            assert!(s.busy, "{event} must set busy");
        }
    }

    #[test]
    fn stop_clears_busy() {
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "PreToolUse", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "Stop", &input("C:/x"), "overmind", 1, now()).unwrap();
        assert!(!s.busy);
    }

    #[test]
    fn subagent_start_increments_and_stop_decrements() {
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "SubagentStart", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "SubagentStart", &input("C:/x"), "overmind", 1, now()).unwrap();
        assert_eq!(s.subagents_running, 2);
        let s = apply_event(
            Some(s),
            "SubagentStop",
            &input("C:/x"),
            "overmind",
            1,
            now(),
        )
        .unwrap();
        assert_eq!(s.subagents_running, 1);
    }

    #[test]
    fn subagent_stop_never_goes_below_zero() {
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "SubagentStop", &input("C:/x"), "overmind", 1, now()).unwrap();
        assert_eq!(s.subagents_running, 0);
    }

    #[test]
    fn post_model_switch_updates_the_model() {
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        let mut with_model = input("C:/x");
        with_model.model = Some("claude-opus-5".to_string());
        let s = apply_event(s, "PostModelSwitch", &with_model, "overmind", 1, now()).unwrap();
        assert_eq!(s.model, Some("claude-opus-5".to_string()));
    }

    #[test]
    fn post_model_switch_with_no_model_field_leaves_it_unchanged() {
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        let s = apply_event(s, "PostModelSwitch", &input("C:/x"), "overmind", 1, now()).unwrap();
        assert_eq!(s.model, None);
    }

    #[test]
    fn no_background_shells_is_never_set_true_by_any_hook_event() {
        // RESTART-TOOL-DESIGN.md §1a: that claim is the lane's own manual
        // assertion when writing HANDOFF, never something a lifecycle
        // hook can honestly assert on the lane's behalf.
        let s = apply_event(None, "SessionStart", &input("C:/x"), "overmind", 1, now());
        for event in [
            "UserPromptSubmit",
            "PreToolUse",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "PostModelSwitch",
        ] {
            let s2 = apply_event(s.clone(), event, &input("C:/x"), "overmind", 1, now()).unwrap();
            assert_ne!(s2.no_background_shells, Some(true));
        }
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
}
