// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the OS actually says, behind one trait — so `authorize.rs`'s decision
//! logic can be tested against a fake without ever touching a real process.
//!
//! ⚠️ `SysinfoFacts` (the real implementation) is NOT unit-tested here - it
//! cannot be, without a real process to observe, which this crate must never
//! spin up just to test itself. It is exercised by `--dry-run` against real
//! lanes before `--yes` is ever used for real. Everything this module's
//! CALLERS do with what it returns - the actual refusal logic - is fully
//! tested in `authorize.rs`.

use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};

/// ⚠️ PM finding, 2026-09-18 (first real restart attempt): `System::refresh_processes` (no
/// `_specifics`) uses a DEFAULT `ProcessRefreshKind` that leaves `cwd` and `cmd` at
/// `UpdateKind::Never` - confirmed by reading sysinfo 0.39.6's own default impl. Every `cwd_of`
/// call in this module was reading an always-empty field, not a genuinely missing one; a real
/// restart's identity check failed closed ("could not read the cwd of pid ...") on every attempt,
/// for a reason that had nothing to do with the pid itself. `exe` happened to still work by luck
/// (the default explicitly sets it to `OnlyIfNotSet`, which fetches it on a fresh, never-yet-set
/// `System`) - `cmd` and `cwd` have no such override and stayed empty. This refresh kind is
/// explicit about all three so none of them depend on an unstated default again.
fn full_process_refresh() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_cwd(UpdateKind::Always)
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
}

/// Image names (lowercased, no extension) counted as "a live shell" when
/// found as a descendant of the target PID — RESTART-TOOL-DESIGN.md §1a.
const SHELL_IMAGE_NAMES: &[&str] = &["bash", "pwsh", "powershell", "cmd", "sh", "zsh", "fish"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellCheckError {
    /// The process table could not be enumerated cleanly. RESTART-TOOL-DESIGN.md
    /// §1a: this counts as UNSAFE, the same as finding a shell - never as "no
    /// shell found."
    EnumerationFailed(String),
}

/// What identifies a PID as "the same process" across two points in time,
/// so a kill can be refused if the PID has since been recycled. PM finding,
/// 2026-09-18: a PID alone is not enough between `decide()` recording it and
/// the actual kill running moments later - a vanishingly unlikely but real
/// window in which the original process could exit and the PID be reused by
/// something else entirely before the signal is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub start_time_secs: u64,
    pub exe: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KillError {
    /// The PID is no longer running at all.
    NoLongerRunning,
    /// A live process exists at this PID, but its start time (or exe path,
    /// when both sides have one) no longer matches what was recorded at
    /// `decide()` time - the PID was almost certainly recycled. Refused,
    /// never killed.
    IdentityChanged,
    SignalRejected(String),
}

impl std::fmt::Display for KillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KillError::NoLongerRunning => write!(f, "the process is no longer running"),
            KillError::IdentityChanged => write!(
                f,
                "the pid's identity changed since it was checked - refusing, likely PID reuse"
            ),
            KillError::SignalRejected(m) => write!(f, "the kill signal was rejected: {m}"),
        }
    }
}

pub trait SystemFacts {
    /// Is `pid` alive right now, and is its own image name `claude` (or
    /// `claude.exe`)? A dead PID, or a live one that isn't Claude Code
    /// itself (PID reuse), both return `false` - see RESTART-TOOL-DESIGN.md §2.1.
    fn is_alive_claude_process(&self, pid: u32) -> bool;

    /// The live process's own working directory, if it could be read.
    fn cwd_of(&self, pid: u32) -> Option<PathBuf>;

    /// §1a's second, independent signal: walk `pid`'s descendants looking
    /// for a live shell. `Ok(true)` = found one, `Ok(false)` = none found
    /// among processes that WERE enumerable, `Err` = the walk itself failed
    /// (treated as unsafe by the caller, same as `Ok(true)`).
    fn has_live_shell_descendant(&self, pid: u32) -> Result<bool, ShellCheckError>;

    /// §2.3: does a transcript for `session_id` exist under `cwd`'s project
    /// storage, with an mtime recent enough (within `max_age`) to plausibly
    /// belong to the CURRENT run rather than a stale leftover from a PID
    /// that got reused?
    fn transcript_is_recent(&self, cwd: &str, session_id: &str, max_age: Duration) -> bool;

    fn now(&self) -> DateTime<Utc>;

    /// The identity to record at `decide()` time, for `kill_verified` to
    /// re-check right before acting. `None` if the pid isn't alive.
    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity>;

    /// Sends the real kill - but ONLY if `pid`'s identity, re-read right
    /// now, still matches `expected`. Refuses (never signals) on any
    /// mismatch, including the process simply no longer existing. Only
    /// ever called after `authorize::decide` returns `Ok`, and only in
    /// non-dry-run mode.
    fn kill_verified(&self, pid: u32, expected: &ProcessIdentity) -> Result<(), KillError>;
}

pub struct SysinfoFacts {
    claude_config_dir: PathBuf,
}

impl SysinfoFacts {
    pub fn new(claude_config_dir: PathBuf) -> Self {
        Self { claude_config_dir }
    }

    fn project_dir_name(cwd: &str) -> String {
        // ⚠️ MATCHES sessions.md: "your working directory path with
        // non-alphanumeric characters replaced by -". Not a guess - quoted
        // directly from the verified documentation this spec cites.
        cwd.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
    }
}

/// `kill_verified`'s exe-path check, pulled out as a pure function so it is
/// unit-testable without a real process (unlike the rest of this module -
/// see its own doc comment). Absence on either side is not itself a
/// mismatch; only two PRESENT paths that fail to normalise-match are.
fn exe_matches(current: &Option<PathBuf>, expected: &Option<PathBuf>) -> bool {
    match (current, expected) {
        (Some(a), Some(b)) => crate::paths::paths_match(&a.to_string_lossy(), &b.to_string_lossy()),
        _ => true,
    }
}

impl SystemFacts for SysinfoFacts {
    fn is_alive_claude_process(&self, pid: u32) -> bool {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            full_process_refresh(),
        );
        match sys.process(Pid::from_u32(pid)) {
            Some(p) => {
                let name = p.name().to_string_lossy().to_lowercase();
                name == "claude" || name == "claude.exe"
            }
            None => false,
        }
    }

    fn cwd_of(&self, pid: u32) -> Option<PathBuf> {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            full_process_refresh(),
        );
        sys.process(Pid::from_u32(pid))
            .and_then(|p| p.cwd())
            .map(|p| p.to_path_buf())
    }

    fn has_live_shell_descendant(&self, pid: u32) -> Result<bool, ShellCheckError> {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            full_process_refresh(),
        );
        let target = Pid::from_u32(pid);
        let mut stack = vec![target];
        let mut seen = std::collections::HashSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            for (child_pid, process) in sys.processes() {
                if process.parent() != Some(current) {
                    continue;
                }
                let name = process.name().to_string_lossy().to_lowercase();
                let base = name.trim_end_matches(".exe");
                if SHELL_IMAGE_NAMES.contains(&base) {
                    return Ok(true);
                }
                stack.push(*child_pid);
            }
        }
        Ok(false)
    }

    fn transcript_is_recent(&self, cwd: &str, session_id: &str, max_age: Duration) -> bool {
        let project = Self::project_dir_name(cwd);
        let path = self
            .claude_config_dir
            .join("projects")
            .join(project)
            .join(format!("{session_id}.jsonl"));
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        match modified.elapsed() {
            Ok(age) => age <= max_age,
            Err(_) => false, // mtime in the future - refuse rather than trust it
        }
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            full_process_refresh(),
        );
        sys.process(Pid::from_u32(pid)).map(|p| ProcessIdentity {
            start_time_secs: p.start_time(),
            exe: p.exe().map(|e| e.to_path_buf()),
        })
    }

    fn kill_verified(&self, pid: u32, expected: &ProcessIdentity) -> Result<(), KillError> {
        // ⚠️ RE-READ, DO NOT TRUST `expected` ALONE. The whole point is that
        // time has passed since `expected` was recorded; a fresh read is
        // what makes this a check rather than a formality.
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            full_process_refresh(),
        );
        let process = sys
            .process(Pid::from_u32(pid))
            .ok_or(KillError::NoLongerRunning)?;
        let current = ProcessIdentity {
            start_time_secs: process.start_time(),
            exe: process.exe().map(|e| e.to_path_buf()),
        };
        if current.start_time_secs != expected.start_time_secs {
            return Err(KillError::IdentityChanged);
        }
        // ⚠️ PM finding, 2026-09-18: the same separator/trailing/case gap
        // `authorize::identify`'s cwd compare had applies here too - a raw
        // `!=` on two `PathBuf`s re-read from sysinfo at different moments
        // could refuse a genuinely matching exe path over formatting, not a
        // real identity change.
        if !exe_matches(&current.exe, &expected.exe) {
            return Err(KillError::IdentityChanged);
        }
        if process.kill() {
            Ok(())
        } else {
            Err(KillError::SignalRejected(format!(
                "kill signal to pid {pid} was not accepted"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_dir_name_matches_the_documented_rule() {
        // sessions.md: "working directory path with non-alphanumeric
        // characters replaced by -"
        assert_eq!(
            SysinfoFacts::project_dir_name("C:/Projects/OverMind"),
            "C--Projects-OverMind"
        );
    }

    // -- exe_matches -------------------------------------------------------- //
    // PM finding, 2026-09-18: kill_verified's exe compare needs the same
    // separator/case normalisation authorize::identify's cwd compare does -
    // pulled into its own pure function so this is testable without a real
    // process, unlike kill_verified itself.

    #[test]
    fn exe_matches_paths_that_differ_only_by_separator() {
        let a = Some(PathBuf::from(r"C:\claude.exe"));
        let b = Some(PathBuf::from("C:/claude.exe"));
        assert!(exe_matches(&a, &b));
    }

    /// ⚠️ `PathBuf`'s OWN `PartialEq` already normalises `/` vs `\` on
    /// Windows - the separator test above would pass even on a raw `a ==
    /// b`, and would NOT have caught a regression back to it. Case is the
    /// part `PathBuf` equality does NOT normalise, so this is the one that
    /// actually proves `exe_matches` goes through `paths::paths_match`
    /// rather than plain `PathBuf` equality.
    #[cfg(windows)]
    #[test]
    fn exe_matches_paths_that_differ_only_by_case_on_windows() {
        let a = Some(PathBuf::from(r"C:\Claude.exe"));
        let b = Some(PathBuf::from(r"C:\claude.exe"));
        assert!(exe_matches(&a, &b));
    }

    #[test]
    fn exe_matches_rejects_a_genuinely_different_path() {
        let a = Some(PathBuf::from(r"C:\claude.exe"));
        let b = Some(PathBuf::from(r"C:\other.exe"));
        assert!(!exe_matches(&a, &b));
    }

    #[test]
    fn exe_matches_when_either_side_is_absent() {
        assert!(exe_matches(&None, &Some(PathBuf::from(r"C:\claude.exe"))));
        assert!(exe_matches(&Some(PathBuf::from(r"C:\claude.exe")), &None));
        assert!(exe_matches(&None, &None));
    }

    /// ⚠️ THE ONE REAL-PROCESS TEST IN THIS MODULE. Safe because it only
    /// ever spawns and kills a child THIS TEST OWNS - never touches
    /// anything else on the system. PM finding, 2026-09-18: `kill_verified`
    /// must re-read the pid's identity and refuse if it no longer matches
    /// what was recorded, so a recycled pid can't be killed by mistake.
    #[test]
    fn kill_verified_refuses_on_a_mismatched_identity_and_succeeds_on_a_matching_one() {
        let mut child = spawn_sleep_child();
        let pid = child.id();
        let facts = SysinfoFacts::new(std::env::temp_dir());

        // Give sysinfo a moment to see the freshly-spawned process.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let real_identity = facts
            .process_identity(pid)
            .expect("the freshly spawned child must be observable");

        let wrong_identity = ProcessIdentity {
            start_time_secs: real_identity.start_time_secs + 999_999,
            exe: real_identity.exe.clone(),
        };
        let refused = facts.kill_verified(pid, &wrong_identity);
        assert_eq!(refused, Err(KillError::IdentityChanged));

        // The child must still be alive - the mismatched call must not
        // have killed it.
        assert!(
            child.try_wait().unwrap().is_none(),
            "a refused kill must not have touched the process"
        );

        let accepted = facts.kill_verified(pid, &real_identity);
        assert_eq!(accepted, Ok(()));
        let _ = child.wait();
    }

    #[test]
    fn kill_verified_refuses_a_pid_that_is_no_longer_running() {
        let mut child = spawn_sleep_child();
        let pid = child.id();
        let facts = SysinfoFacts::new(std::env::temp_dir());
        std::thread::sleep(std::time::Duration::from_millis(200));
        let identity = facts.process_identity(pid).unwrap();

        child.kill().unwrap();
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(200));

        assert_eq!(
            facts.kill_verified(pid, &identity),
            Err(KillError::NoLongerRunning)
        );
    }

    /// ⚠️ THE TEST THAT WOULD HAVE CAUGHT THE MISSING-FIELDS BUG. PM finding,
    /// 2026-09-18 (first real restart attempt): `cwd_of` always returned
    /// `None` in production because plain `refresh_processes` never
    /// populates `cwd` - but every existing test used `FakeFacts`, which
    /// can't see that. This spawns a REAL child with a KNOWN cwd and reads
    /// it back through `SysinfoFacts` itself, so "never set in production"
    /// can't hide behind a fake again.
    #[test]
    fn cwd_of_reads_a_real_spawned_childs_actual_working_directory() {
        let known_dir = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir must be readable");
        let mut child = spawn_sleep_child_in(&known_dir);
        let pid = child.id();
        let facts = SysinfoFacts::new(std::env::temp_dir());

        std::thread::sleep(std::time::Duration::from_millis(200));
        let observed = facts
            .cwd_of(pid)
            .expect("a real spawned child's cwd must be readable, not None");

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            observed.canonicalize().expect("observed cwd must exist"),
            known_dir,
            "cwd_of must read the child's REAL working directory, not an \
             empty/default one"
        );
    }

    /// ⚠️ THE REAL-WORLD CASE, REPRODUCED FOR REAL: PM finding, 2026-09-18 -
    /// a real Windows process's own `cwd` carries a trailing separator; the
    /// hook's recorded `cwd` (this test's stand-in: the same path with the
    /// separator stripped, the way a hook payload's `cwd` field arrives)
    /// never does. `cwd_of`'s raw string is read from a REAL spawned child
    /// here, not constructed by hand, so this can't pass for a reason
    /// unrelated to what `paths_match` actually has to reconcile.
    #[test]
    fn cwd_of_a_real_child_matches_the_same_path_without_a_trailing_separator() {
        let known_dir = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir must be readable");
        let mut child = spawn_sleep_child_in(&known_dir);
        let pid = child.id();
        let facts = SysinfoFacts::new(std::env::temp_dir());

        std::thread::sleep(std::time::Duration::from_millis(200));
        let observed = facts
            .cwd_of(pid)
            .expect("a real spawned child's cwd must be readable, not None");

        let _ = child.kill();
        let _ = child.wait();

        let observed_str = observed.to_string_lossy().to_string();
        let without_trailing_sep = observed_str.trim_end_matches(['\\', '/']).to_string();
        assert!(
            crate::paths::paths_match(&observed_str, &without_trailing_sep),
            "a real process's own cwd ({observed_str:?}) must match the same \
             path with its trailing separator stripped ({without_trailing_sep:?})"
        );
    }

    fn spawn_sleep_child_in(dir: &std::path::Path) -> std::process::Child {
        #[cfg(windows)]
        let child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .spawn();
        #[cfg(not(windows))]
        let child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(dir)
            .spawn();
        child.expect("could not spawn a throwaway child process for this test")
    }

    fn spawn_sleep_child() -> std::process::Child {
        #[cfg(windows)]
        let child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn();
        #[cfg(not(windows))]
        let child = std::process::Command::new("sleep").arg("30").spawn();
        child.expect("could not spawn a throwaway child process for this test")
    }
}
