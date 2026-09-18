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
use sysinfo::{Pid, System};

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

impl SystemFacts for SysinfoFacts {
    fn is_alive_claude_process(&self, pid: u32) -> bool {
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
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
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        sys.process(Pid::from_u32(pid))
            .and_then(|p| p.cwd())
            .map(|p| p.to_path_buf())
    }

    fn has_live_shell_descendant(&self, pid: u32) -> Result<bool, ShellCheckError> {
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
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
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
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
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
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
        if let (Some(a), Some(b)) = (&current.exe, &expected.exe) {
            if a != b {
                return Err(KillError::IdentityChanged);
            }
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
