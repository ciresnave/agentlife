// SPDX-License-Identifier: MIT OR Apache-2.0
//! Identifying a process: **pid plus process start time**, never a pid alone.
//!
//! OS pids are recycled, so a recorded pid can name a different process later (DESIGN.md §2.4,
//! the OverMind spec §2 four-part check). The start time is the part that tells two processes
//! with the same pid apart. `lane-restart` records it as `LaneState.pid_start_secs` (seconds
//! since the epoch, from `sysinfo`); this module uses the same unit so the two agree.
//!
//! **How exact this is.** On Windows the start time is the process's creation time, so the pair is
//! exact. On other platforms `sysinfo` derives it from the boot time and the process's start tick, and
//! the boot time can move by a second as the clock is adjusted (a hypothesis, **unproven**: one Linux CI
//! test failed once in about thirty runs as if two reads of a live process had disagreed by a second).
//! Start-time identity is therefore **exact on Windows and best-effort elsewhere**; a green Linux CI leg
//! is not a Linux guarantee. No tolerance is applied: it would widen the pid-recycle window.
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

/// Whether `child` is `root` itself or somewhere below it, by path text alone (no disk access,
/// so a symlink is not followed: a caller that needs that must check separately). A `..` anywhere
/// in `child` is refused, because `C:/Projects/../Windows` is textually below `C:/Projects`.
/// **Never a prefix match**: `C:/Projects2` is not under `C:/Projects`.
pub fn path_is_under(child: &str, root: &str) -> bool {
    let c = normalise(child);
    if c.split('/').any(|part| part == "..") {
        return false;
    }
    let r = normalise(root);
    let r_slash = if r.ends_with('/') {
        r.clone()
    } else {
        format!("{r}/")
    };
    c == r || c.starts_with(&r_slash)
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

/// A process that has been killed but not yet reaped by its parent (a **zombie** on Linux; Windows
/// has none) still has an entry in the process table. It is not running. Treating it as running made
/// a stop that had WORKED report "still running after 5 s" and put the intent back: found when the
/// graceful-stop end-to-end test failed on the Ubuntu CI leg in 6 of 6 attempts, because the test
/// is the dead process's parent and had not yet waited for it.
pub fn process_is_dead(p: &sysinfo::Process) -> bool {
    matches!(
        p.status(),
        sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
    )
}

/// The real process table, via `sysinfo`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SysinfoTable;

/// Reads taken of a pid whose image reads as the empty path before the answer is believed.
const IMAGE_READS: u32 = 40;
/// The pause between those reads: the whole wait is at most 1 s, and only for a pid in that state.
const IMAGE_PAUSE: std::time::Duration = std::time::Duration::from_millis(25);

/// `Some("")`, as `sysinfo` reports it on Windows when the image cannot be read at all
/// (`get_exe` ignores the failure of `GetModuleFileNameExW`). A live process whose image we may
/// read never has it; a process that is exiting or has just exited does, for a moment, while the
/// OS still lists it. An empty path is neither "a different exe" nor "gone".
fn image_unreadable(i: &ProcessIdentity) -> bool {
    i.exe.as_deref() == Some("")
}

/// Re-reads while the image is unreadable, so a process caught between exit and removal from the
/// table settles into either "gone" (`None`) or its real image. If it is still unreadable after
/// `reads` reads, the empty image is returned as it is: the result then never matches a recorded
/// exe, which is the safe direction (never `Same`, never a pid taken for a live lane).
fn settle_image(
    mut read: impl FnMut() -> Option<ProcessIdentity>,
    reads: u32,
    pause: std::time::Duration,
) -> Option<ProcessIdentity> {
    let mut last = read();
    for _ in 1..reads {
        if !last.as_ref().is_some_and(image_unreadable) {
            break;
        }
        std::thread::sleep(pause);
        last = read();
    }
    last
}

impl ProcessTable for SysinfoTable {
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
        settle_image(|| self.read_once(pid), IMAGE_READS, IMAGE_PAUSE)
    }
}

impl SysinfoTable {
    fn read_once(&self, pid: u32) -> Option<ProcessIdentity> {
        let Ok(mut sys) = System::new() else {
            return None;
        };
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
        if process_is_dead(proc_) {
            return None;
        }
        Some(ProcessIdentity {
            pid,
            start_secs: proc_.start_time(),
            exe: proc_.exe().map(|e| e.display().to_string()),
        })
    }
}

/// One refresh of the whole process table, then any number of lookups. `list` checks every
/// agent, and refreshing per agent would repeat the same system walk hundreds of times.
#[derive(Debug, Default)]
pub struct SnapshotTable(std::collections::HashMap<u32, ProcessIdentity>);

impl SnapshotTable {
    /// Pid and start time only: no executable path. Reading every process's image is the slow
    /// part of a refresh, and every caller here compares start times; `check` skips the exe
    /// comparison when either side has none.
    pub fn capture() -> Self {
        let Ok(mut sys) = System::new() else {
            return Self(Default::default());
        };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        Self(
            sys.processes()
                .iter()
                .filter(|(_, p)| !process_is_dead(p))
                .map(|(pid, p)| {
                    let pid = pid.as_u32();
                    (
                        pid,
                        ProcessIdentity {
                            pid,
                            start_secs: p.start_time(),
                            exe: None,
                        },
                    )
                })
                .collect(),
        )
    }
}

impl ProcessTable for SnapshotTable {
    fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
        self.0.get(&pid).cloned()
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
    fn path_is_under_is_textual_never_a_prefix_and_refuses_dot_dot() {
        assert!(path_is_under("C:/Projects/x", "C:/Projects"));
        assert!(path_is_under(r"C:\Projects\x\y\", "C:/Projects"));
        assert!(
            path_is_under("C:/Projects", "C:/Projects"),
            "the root itself"
        );
        assert!(!path_is_under("C:/Projects2/x", "C:/Projects"));
        assert!(!path_is_under("C:/Projects/../Windows", "C:/Projects"));
        assert!(!path_is_under("C:/Other", "C:/Projects"));
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
        // Between fork and exec a child still shows its PARENT's image, and on the Ubuntu CI
        // leg this test once read exactly that (`exe was .../agentlife-<hash>`, the test binary).
        // So poll until the image becomes the program we started, and if it never does, say
        // everything that was seen, so the explanation is checked instead of assumed.
        let mut seen: Vec<String> = Vec::new();
        let mut converged = false;
        for _ in 0..40 {
            if let Some(i) = SysinfoTable.identity_of(pid) {
                let exe = i.exe.unwrap_or_default();
                let lower = exe.to_lowercase();
                if lower.contains("ping") || lower.contains("sleep") {
                    converged = true;
                    break;
                }
                if seen.last() != Some(&exe) {
                    seen.push(exe);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            converged,
            "the child's exe never became ping/sleep; images seen over 2 s: {seen:?}"
        );
        // Re-read now that the image has settled. `got` above was taken at spawn, possibly in the
        // fork-to-exec window, and comparing THAT with the settled process is a (correct)
        // `DifferentExe`: attempt 6 on the Ubuntu CI leg did exactly this. The start time is the
        // same either way, which is why the registry records identities WITHOUT an exe.
        let settled = SysinfoTable
            .identity_of(pid)
            .expect("the child is still alive");
        assert_eq!(
            settled.start_secs, got.start_secs,
            "an exec does not change the start time"
        );
        let got = settled;

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

    fn ident(exe: Option<&str>) -> ProcessIdentity {
        ProcessIdentity {
            pid: 7,
            start_secs: 100,
            exe: exe.map(str::to_string),
        }
    }

    /// Runs `settle_image` over a scripted sequence of reads (the last one repeats), counting them.
    fn settle(script: &[Option<ProcessIdentity>], reads: u32) -> (Option<ProcessIdentity>, u32) {
        let n = std::cell::Cell::new(0usize);
        let got = settle_image(
            || {
                let i = n.get();
                n.set(i + 1);
                script[i.min(script.len() - 1)].clone()
            },
            reads,
            std::time::Duration::ZERO,
        );
        (got, n.get() as u32)
    }

    #[test]
    fn an_empty_image_that_then_vanishes_is_gone_not_a_different_exe() {
        // The #221 failure, scripted: the OS still lists the exited process with an empty image,
        // then drops it. Verdict through `check` is NotRunning.
        let (got, reads) = settle(&[Some(ident(Some(""))), Some(ident(Some(""))), None], 40);
        assert_eq!((got.clone(), reads), (None, 3));
        let table = Scripted(got);
        let mut expected = ident(Some("C:\\Windows\\System32\\PING.EXE"));
        expected.start_secs = 100;
        assert_eq!(check(&table, &expected), Match::NotRunning);
    }

    #[test]
    fn an_empty_image_that_then_reads_is_the_real_process() {
        let real = ident(Some("C:\\a\\b.exe"));
        let (got, reads) = settle(&[Some(ident(Some(""))), Some(real.clone())], 40);
        assert_eq!((got, reads), (Some(real), 2));
    }

    #[test]
    fn a_readable_gone_or_unknown_image_is_one_read_and_a_stuck_empty_one_stays_empty() {
        let real = ident(Some("C:\\a\\b.exe"));
        assert_eq!(settle(&[Some(real.clone())], 40), (Some(real), 1));
        assert_eq!(settle(&[None], 40), (None, 1));
        // `None` is "the OS would not say", not "empty": no waiting, and the comparison is skipped.
        let unknown = ident(None);
        assert_eq!(settle(&[Some(unknown.clone())], 40), (Some(unknown), 1));
        // Still empty after every read: returned as it is (never Same against a recorded exe).
        let empty = ident(Some(""));
        assert_eq!(settle(&[Some(empty.clone())], 5), (Some(empty.clone()), 5));
        let mut expected = ident(Some("C:\\a\\b.exe"));
        expected.start_secs = 100;
        assert!(matches!(
            check(&Scripted(Some(empty)), &expected),
            Match::DifferentExe { .. }
        ));
    }

    struct Scripted(Option<ProcessIdentity>);
    impl ProcessTable for Scripted {
        fn identity_of(&self, _pid: u32) -> Option<ProcessIdentity> {
            self.0.clone()
        }
    }

    /// The verdict on a real child the instant after it is killed and reaped, `rounds` times over,
    /// each round a fresh child. A killed pid is `NotRunning`: never `DifferentExe`, never `Same`.
    /// Found 2026-11-01 on a Windows CI leg of #221 (`DifferentExe { actual: "" }`): for a moment
    /// after exit the OS still lists the process, and `sysinfo` reads its image as the EMPTY path
    /// (`get_exe` ignores the failure of `GetModuleFileNameExW`), which is not "a different exe"
    /// and not "gone".
    fn verdicts_for_killed_children(rounds: usize) -> Vec<Match> {
        (0..rounds)
            .map(|_| {
                let mut child = long_running_child();
                let pid = child.id();
                // The recorded identity carries the exe, as the strict callers' do: wait for the
                // image to become the program we started (fork-to-exec shows the parent's).
                let mut expected = None;
                for _ in 0..80 {
                    if let Some(i) = SysinfoTable.identity_of(pid) {
                        let exe = i.exe.clone().unwrap_or_default().to_lowercase();
                        if exe.contains("ping") || exe.contains("sleep") {
                            expected = Some(i);
                            break;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                let expected = expected.expect("the child's image became ping/sleep");
                child.kill().unwrap();
                child.wait().unwrap();
                check(&SysinfoTable, &expected)
            })
            .collect()
    }

    #[test]
    fn a_killed_child_is_never_a_different_exe_over_many_real_rounds() {
        // Eight threads at once: the failure was seen on a CI runner running the whole suite in
        // parallel, and on an idle machine one thread did not show it in 450 rounds.
        let (threads, per_thread) = (8, 40);
        let rounds = threads * per_thread;
        let handles: Vec<_> = (0..threads)
            .map(|_| std::thread::spawn(move || verdicts_for_killed_children(per_thread)))
            .collect();
        let verdicts: Vec<Match> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("a round panicked"))
            .collect();
        let bad: Vec<&Match> = verdicts
            .iter()
            .filter(|v| **v != Match::NotRunning)
            .collect();
        assert!(
            bad.is_empty(),
            "{} of {rounds} killed children were not NotRunning: {bad:?}",
            bad.len()
        );
    }

    #[test]
    fn a_snapshot_agrees_with_the_per_pid_table_about_a_real_process() {
        let mut child = long_running_child();
        let pid = child.id();
        let snap = SnapshotTable::capture();
        let from_snap = snap.identity_of(pid).expect("the child is in the snapshot");
        let direct = SysinfoTable
            .identity_of(pid)
            .expect("and in the direct lookup");
        assert_eq!(from_snap.start_secs, direct.start_secs);
        assert!(
            snap.identity_of(std::process::id()).is_some(),
            "it also sees this test process"
        );
        assert_eq!(snap.identity_of(u32::MAX - 1), None);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn the_real_table_returns_nothing_for_a_pid_that_cannot_exist() {
        assert_eq!(SysinfoTable.identity_of(u32::MAX - 1), None);
    }

    /// A killed process its parent has not reaped is a zombie: still in the table, not running.
    /// Only Linux has them, so this runs only there (and only CI runs it).
    #[cfg(unix)]
    #[test]
    fn a_zombie_is_not_running_in_either_real_table() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("start a child that exits at once");
        let pid = child.id();
        // Do NOT wait for it: that is what keeps it a zombie. Give it time to exit.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(
            SysinfoTable.identity_of(pid),
            None,
            "a zombie is not running"
        );
        assert_eq!(
            SnapshotTable::capture().identity_of(pid),
            None,
            "nor in a snapshot"
        );
        child.wait().unwrap();
    }
}
