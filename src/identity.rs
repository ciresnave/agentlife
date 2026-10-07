// SPDX-License-Identifier: MIT OR Apache-2.0
//! Identifying a process: **pid plus process start time**, never a pid alone.
//!
//! OS pids are recycled, so a recorded pid can name a different process later (DESIGN.md §2.4,
//! the OverMind spec §2 four-part check). The start time is the part that tells two processes
//! with the same pid apart. `lane-restart` records it as `LaneState.pid_start_secs` (seconds
//! since the epoch, from `sysinfo`); this module uses the same unit so the two agree.
//!
//! Reading a process goes through [`ProcessTable`], so every decision built on it is testable
//! with a fake, and [`SysinfoTable`] is proven against a **real spawned child** (the one kind of
//! test a fake structurally cannot replace: OverMind's `cwd`/`cmd` bug was invisible to 79 passing
//! fake-based tests until two real-child tests existed).

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// Seconds since the Unix epoch.
    pub start_secs: u64,
    /// The executable path, when the OS lets us read it.
    pub exe: Option<String>,
}

pub trait ProcessTable {
    /// The identity of the process currently holding `pid`, or `None` if nothing does.
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity>;
}

/// The verdict of comparing a recorded identity with what holds that pid now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Same,
    /// Nothing is running under that pid.
    NotRunning,
    /// Something is, but it started at a different time: a recycled pid.
    RecycledPid {
        expected_start: u64,
        actual_start: u64,
    },
    /// Same start time but a different executable (checked only when both are known).
    DifferentExe {
        expected: String,
        actual: String,
    },
}

/// Compares `expected` with the process now holding its pid.
pub fn check(table: &dyn ProcessTable, expected: &ProcessIdentity) -> Match {
    let Some(now) = table.identity_of(expected.pid) else {
        return Match::NotRunning;
    };
    if now.start_secs != expected.start_secs {
        return Match::RecycledPid {
            expected_start: expected.start_secs,
            actual_start: now.start_secs,
        };
    }
    if let (Some(e), Some(a)) = (&expected.exe, &now.exe) {
        if !paths_equal(e, a) {
            return Match::DifferentExe {
                expected: e.clone(),
                actual: a.clone(),
            };
        }
    }
    Match::Same
}

/// True only for [`Match::Same`].
pub fn is_same(table: &dyn ProcessTable, expected: &ProcessIdentity) -> bool {
    check(table, expected) == Match::Same
}

/// Path equality without touching the disk: unifies `/` and `\`, drops a `\\?\` prefix and one
/// trailing separator (except a bare drive root, where it is significant), and ignores case on
/// Windows, where the filesystem does. **Never a prefix match**: `C:\a` and `C:\ab` differ.
/// (The rule `lane-restart`'s `paths_match` settled on after a real refusal over a trailing
/// separator.)
pub fn paths_equal(a: &str, b: &str) -> bool {
    normalise(a) == normalise(b)
}

fn normalise(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    }
    let is_drive_root = s.len() == 3 && s.as_bytes()[1] == b':' && s.ends_with('/');
    if s.len() > 1 && s.ends_with('/') && !is_drive_root {
        s.pop();
    }
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// The real process table, via `sysinfo`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SysinfoTable;

impl ProcessTable for SysinfoTable {
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
        let mut sys = System::new();
        let p = Pid::from_u32(pid);
        // Explicit, never the default: sysinfo's default refresh leaves `exe`, `cwd` and `cmd`
        // unset, which is exactly the bug that made every real identity check in OverMind fail
        // while its fake-based tests passed.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[p]),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        let proc_ = sys.process(p)?;
        Some(ProcessIdentity {
            pid,
            start_secs: proc_.start_time(),
            exe: proc_.exe().map(|e| e.display().to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Fake(HashMap<u32, ProcessIdentity>);
    impl ProcessTable for Fake {
        fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
            self.0.get(&pid).cloned()
        }
    }

    fn id(pid: u32, start: u64, exe: Option<&str>) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_secs: start,
            exe: exe.map(str::to_string),
        }
    }

    fn table(p: ProcessIdentity) -> Fake {
        Fake(HashMap::from([(p.pid, p)]))
    }

    #[test]
    fn the_same_pid_and_start_time_is_the_same_process() {
        let t = table(id(10, 1000, Some("C:/x/claude.exe")));
        assert_eq!(
            check(&t, &id(10, 1000, Some("C:\\x\\claude.exe"))),
            Match::Same,
            "a different separator style alone must not make two paths differ, on any OS"
        );
        assert!(
            is_same(&t, &id(10, 1000, None)),
            "an unknown exe does not block a match"
        );
    }

    #[test]
    fn a_recycled_pid_is_not_the_same_process() {
        // The pid is alive, but it started later than the recorded process did.
        let t = table(id(10, 5000, Some("C:/x/claude.exe")));
        assert_eq!(
            check(&t, &id(10, 1000, Some("C:/x/claude.exe"))),
            Match::RecycledPid {
                expected_start: 1000,
                actual_start: 5000
            }
        );
        assert!(!is_same(&t, &id(10, 1000, None)));
    }

    #[test]
    fn a_pid_nothing_holds_is_not_running() {
        let t = Fake(HashMap::new());
        assert_eq!(check(&t, &id(10, 1000, None)), Match::NotRunning);
    }

    #[test]
    fn same_start_time_but_a_different_exe_is_refused() {
        let t = table(id(10, 1000, Some("C:/x/other.exe")));
        assert!(matches!(
            check(&t, &id(10, 1000, Some("C:/x/claude.exe"))),
            Match::DifferentExe { .. }
        ));
    }

    #[test]
    fn paths_equal_ignores_separator_style_and_one_trailing_separator() {
        assert!(paths_equal("C:\\Projects\\x\\", "C:/Projects/x"));
        assert!(paths_equal("\\\\?\\C:\\a\\b.exe", "C:/a/b.exe"));
        assert!(paths_equal("C:\\", "C:/"));
    }

    #[test]
    fn paths_equal_never_matches_a_prefix() {
        assert!(!paths_equal("C:/a", "C:/ab"));
        assert!(!paths_equal("C:/a/b", "C:/a"));
        assert!(
            !paths_equal("C:/", "C:"),
            "a drive root's separator is significant"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_exe_comparison_ignores_case_on_windows() {
        let t = table(id(10, 1000, Some("C:/x/claude.exe")));
        assert_eq!(
            check(&t, &id(10, 1000, Some("C:\\X\\CLAUDE.EXE"))),
            Match::Same
        );
    }

    /// The Linux counterpart: there paths are case-sensitive, so a different case really is a
    /// different executable. (The first version of the shared test assumed otherwise and failed
    /// on the Ubuntu CI leg.)
    #[cfg(not(windows))]
    #[test]
    fn the_exe_comparison_is_case_sensitive_off_windows() {
        let t = table(id(10, 1000, Some("/x/claude")));
        assert!(matches!(
            check(&t, &id(10, 1000, Some("/X/claude"))),
            Match::DifferentExe { .. }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn paths_equal_ignores_case_on_windows() {
        assert!(paths_equal("C:/Projects/X", "c:/projects/x"));
    }

    fn long_running_child() -> std::process::Child {
        #[cfg(windows)]
        let mut c = std::process::Command::new("ping");
        #[cfg(windows)]
        c.args(["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut c = std::process::Command::new("sleep");
        #[cfg(not(windows))]
        c.arg("30");
        c.stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn a long-running child")
    }

    #[test]
    fn the_real_table_identifies_a_real_spawned_child_and_notices_it_die() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut child = long_running_child();
        let pid = child.id();
        let got = SysinfoTable.identity_of(pid).expect("the child is running");
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(got.pid, pid);
        assert!(
            got.start_secs + 3 >= before && got.start_secs <= after + 3,
            "start time {} should be between {before} and {after}",
            got.start_secs
        );
        let exe = got
            .exe
            .clone()
            .expect("exe must be readable for our own child");
        let lower = exe.to_lowercase();
        assert!(
            lower.contains("ping") || lower.contains("sleep"),
            "exe was {exe}"
        );

        // The real table agrees with itself, and disagrees about a recycled pid.
        assert_eq!(check(&SysinfoTable, &got), Match::Same);
        let mut forged = got.clone();
        forged.start_secs = got.start_secs.saturating_sub(1000);
        assert!(matches!(
            check(&SysinfoTable, &forged),
            Match::RecycledPid { .. }
        ));

        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(check(&SysinfoTable, &got), Match::NotRunning);
    }

    #[test]
    fn the_real_table_returns_nothing_for_a_pid_that_cannot_exist() {
        assert_eq!(SysinfoTable.identity_of(u32::MAX - 1), None);
    }
}
