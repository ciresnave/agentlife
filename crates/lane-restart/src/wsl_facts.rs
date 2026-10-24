// SPDX-License-Identifier: MIT OR Apache-2.0

//! What lane-restart learns about a WSL lane (docs/LANE-RESTART-WSL.md section 5): the
//! [`SystemFacts`] a decision is made through, for a lane whose `claude` is a Linux process inside
//! a distro and whose Windows-side process is the `wsl.exe` the host spawned.
//!
//! The only thing run inside the distro is ONE fixed command, [`ps_argv`], whose output is data.
//! Everything that takes a decision is a pure function of that output ([`parse_ps`],
//! [`lane_claude`], [`has_shell_descendant`]), so none of it needs a distro to test. Anything the
//! probe cannot answer is a refusal, never "nothing found": a stop that cannot confirm the lane is
//! gone says so.

use crate::wsl::WslTarget;
use chrono::{DateTime, Utc};
use lane_state::facts::{KillError, ProcEntry, ProcessIdentity, ShellCheckError, SystemFacts};
use std::path::PathBuf;
use std::time::Duration;

/// Runs a fixed argv and returns its stdout. The real one spawns `wsl.exe`; tests answer from a
/// script. An `Err` is a command that could not run or ended badly.
pub trait DistroRunner {
    fn run(&self, argv: &[String]) -> Result<String, String>;
}

/// The one command lane-restart runs inside a distro: the lane user's processes with pid, parent,
/// elapsed seconds and full command line. Fixed argv, no wrapper, no shell, and the only inputs
/// are the target's `distro` and `user`, both shape-checked. `etimes` (seconds since start) is
/// used rather than `lstart` because `lstart` is the distro's local time and its zone is unknown
/// to Windows. **UNVERIFIED:** that this distro's `ps` has `etimes` (P4 measured `lstart`); if it
/// does not, `ps` fails and every answer below is a refusal.
pub fn ps_argv(target: &WslTarget) -> Vec<String> {
    [
        "wsl.exe",
        "-d",
        target.distro.as_str(),
        "-u",
        target.user.as_str(),
        "--exec",
        "/bin/ps",
        "-u",
        target.user.as_str(),
        "-o",
        "pid=,ppid=,etimes=,args=",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// One row of the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslProc {
    pub pid: u32,
    pub ppid: u32,
    pub elapsed_secs: u64,
    /// The command line split on whitespace; `args[0]` is the program as it was started.
    pub args: Vec<String>,
}

/// Reads the probe's output. A line that is not `pid ppid etimes args...` is an error for the
/// whole answer, as is an empty answer (`ps` always lists itself): a half-read process table is
/// not evidence that anything is absent.
pub fn parse_ps(out: &str) -> Result<Vec<WslProc>, String> {
    let mut procs = Vec::new();
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let mut it = line.split_whitespace();
        let mut num = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|t| t.parse::<u64>().ok())
                .ok_or_else(|| format!("no {what} in the ps line {line:?}"))
        };
        let pid = num("pid")?;
        let ppid = num("ppid")?;
        let elapsed_secs = num("elapsed seconds")?;
        let args: Vec<String> = it.map(str::to_string).collect();
        if args.is_empty() {
            return Err(format!("no command in the ps line {line:?}"));
        }
        procs.push(WslProc {
            pid: u32::try_from(pid).map_err(|_| format!("pid out of range in {line:?}"))?,
            ppid: u32::try_from(ppid).map_err(|_| format!("ppid out of range in {line:?}"))?,
            elapsed_secs,
            args,
        });
    }
    if procs.is_empty() {
        return Err("the ps answer was empty".to_string());
    }
    Ok(procs)
}

fn program_name(arg0: &str) -> &str {
    arg0.rsplit('/').next().unwrap_or(arg0)
}

/// The last component of a Windows or POSIX path, split on both separators on every OS:
/// `Path::file_name` does not treat `\` as a separator when this runs on Linux (CI's Ubuntu leg).
fn image_name(path: &std::path::Path) -> String {
    let full = path.to_string_lossy();
    full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
}

/// Seconds of slack when comparing a start time rebuilt from `etimes` (whole seconds, read a
/// moment after the clock) with one read from the process table.
const START_SLACK_SECS: u64 = 2;

/// The lane's own `claude`: a process whose program is `claude` (never `bwrap`: its command line
/// ENDS with the lane's argv, so a match on `--name` alone would take it for the lane, measured
/// 2026-10-24), whose args say `--name <role>`, and that started no earlier than
/// `not_before_start_secs` (the host's `wsl.exe`, or the moment of a kill). When several qualify
/// the newest wins. `now_secs` is the clock the elapsed seconds were read against.
pub fn lane_claude<'a>(
    procs: &'a [WslProc],
    role: &str,
    not_before_start_secs: u64,
    now_secs: u64,
) -> Option<&'a WslProc> {
    procs
        .iter()
        .filter(|p| p.args.first().is_some_and(|a| program_name(a) == "claude"))
        .filter(|p| p.args.windows(2).any(|w| w[0] == "--name" && w[1] == role))
        .filter(|p| {
            now_secs.saturating_sub(p.elapsed_secs) + START_SLACK_SECS >= not_before_start_secs
        })
        .min_by_key(|p| p.elapsed_secs)
}

const SHELLS: &[&str] = &[
    "bash", "sh", "zsh", "fish", "dash", "ash", "ksh", "csh", "tcsh",
];

fn is_shell(arg0: &str) -> bool {
    let name = program_name(arg0).trim_start_matches('-');
    SHELLS.contains(&name)
}

/// Whether any descendant of `root_pid` (through `ppid`) is a shell. Cycle-safe.
pub fn has_shell_descendant(procs: &[WslProc], root_pid: u32) -> bool {
    let mut frontier = vec![root_pid];
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = frontier.pop() {
        if !seen.insert(parent) {
            continue;
        }
        for p in procs.iter().filter(|p| p.ppid == parent && p.pid != parent) {
            if p.args.first().is_some_and(|a| is_shell(a)) {
                return true;
            }
            frontier.push(p.pid);
        }
    }
    false
}

/// The facts for one WSL lane.
///
/// `inner` answers for the Windows side (the `wsl.exe` host child: its identity, the kill, the
/// clock, the process table); the probe answers for the distro; the transcript is read where
/// Windows can see the distro's files (`transcript_root`, the lane user's `.claude` directory
/// under `\\wsl.localhost\<distro>`), read-only.
pub struct WslFacts<I: SystemFacts, R: DistroRunner> {
    inner: I,
    runner: R,
    target: WslTarget,
    role: String,
    cwd: String,
    transcript_root: PathBuf,
    sleep: Box<dyn Fn(Duration)>,
    confirm_attempts: u32,
    confirm_step: Duration,
}

impl<I: SystemFacts, R: DistroRunner> WslFacts<I, R> {
    pub fn new(
        inner: I,
        runner: R,
        target: WslTarget,
        role: &str,
        cwd: &str,
        transcript_root: PathBuf,
    ) -> Self {
        Self {
            inner,
            runner,
            target,
            role: role.to_string(),
            cwd: cwd.to_string(),
            transcript_root,
            sleep: Box::new(std::thread::sleep),
            confirm_attempts: 6,
            confirm_step: Duration::from_secs(1),
        }
    }

    /// Replaces the wait between the probes that confirm a kill (tests pass a recorder).
    pub fn with_sleep(
        mut self,
        sleep: Box<dyn Fn(Duration)>,
        attempts: u32,
        step: Duration,
    ) -> Self {
        self.sleep = sleep;
        self.confirm_attempts = attempts;
        self.confirm_step = step;
        self
    }

    fn probe(&self) -> Result<Vec<WslProc>, String> {
        let out = self.runner.run(&ps_argv(&self.target))?;
        parse_ps(&out)
    }

    fn now_secs(&self) -> u64 {
        u64::try_from(self.inner.now().timestamp()).unwrap_or(0)
    }

    /// `pid` is the host's `wsl.exe`: alive, and its image really is `wsl.exe`. Returns its
    /// identity. An unreadable image name is a refusal.
    fn host_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        let id = self.inner.process_identity(pid)?;
        let is_wsl = id
            .exe
            .as_deref()
            .is_some_and(|e| image_name(e).eq_ignore_ascii_case("wsl.exe"));
        is_wsl.then_some(id)
    }
}

impl<I: SystemFacts, R: DistroRunner> SystemFacts for WslFacts<I, R> {
    /// Alive means BOTH: the host's `wsl.exe` is alive and the probe finds the lane's `claude`
    /// that started after it. Either alone is not alive; a probe that fails is not alive.
    fn is_alive_claude_process(&self, pid: u32) -> bool {
        let Some(host) = self.host_identity(pid) else {
            return false;
        };
        match self.probe() {
            Ok(procs) => {
                lane_claude(&procs, &self.role, host.start_time_secs, self.now_secs()).is_some()
            }
            Err(_) => false,
        }
    }

    /// The recorded POSIX `cwd`, while the host is alive. The live cwd is not read: a WSL lane's
    /// process is in another kernel, and the recorded one is the launch directory (design 2).
    fn cwd_of(&self, pid: u32) -> Option<PathBuf> {
        self.host_identity(pid)?;
        Some(PathBuf::from(&self.cwd))
    }

    /// A shell below the lane's `claude`, from the probe. The lane not being found, or the probe
    /// failing, is an error (which blocks a stop), never "no shell".
    fn has_live_shell_descendant(&self, pid: u32) -> Result<bool, ShellCheckError> {
        let host = self.host_identity(pid).ok_or_else(|| {
            ShellCheckError::EnumerationFailed("the host's wsl.exe is not running".to_string())
        })?;
        let procs = self.probe().map_err(ShellCheckError::EnumerationFailed)?;
        let claude = lane_claude(&procs, &self.role, host.start_time_secs, self.now_secs())
            .ok_or_else(|| {
                ShellCheckError::EnumerationFailed(format!(
                    "the probe found no claude named {} below the host",
                    self.role
                ))
            })?;
        Ok(has_shell_descendant(&procs, claude.pid))
    }

    /// The transcript as Windows sees the distro's files, read-only. Missing, unreadable or
    /// dated in the future all answer `false`.
    fn transcript_is_recent(&self, cwd: &str, session_id: &str, max_age: Duration) -> bool {
        let path = self
            .transcript_root
            .join("projects")
            .join(lane_state::paths::project_dir_name(cwd))
            .join(format!("{session_id}.jsonl"));
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            return false;
        };
        modified.elapsed().is_ok_and(|age| age <= max_age)
    }

    fn now(&self) -> DateTime<Utc> {
        self.inner.now()
    }

    /// The identity of the host's `wsl.exe` (what a kill re-checks).
    fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        self.host_identity(pid)
    }

    /// Kills the host's `wsl.exe` by the verified identity, then requires the probe to show that
    /// the lane's `claude` is gone. The sandbox ends with `wsl.exe` only because the wrapper says
    /// `--die-with-parent` (measured 2026-10-24 with a stand-in); a wrapper that does not leaves a
    /// survivor, which is reported, not retried blindly.
    fn kill_verified(&self, pid: u32, expected: &ProcessIdentity) -> Result<(), KillError> {
        self.inner.kill_verified(pid, expected)?;
        let mut last = String::new();
        for attempt in 0..self.confirm_attempts.max(1) {
            if attempt > 0 {
                (self.sleep)(self.confirm_step);
            }
            match self.probe() {
                Ok(procs) => {
                    match lane_claude(
                        &procs,
                        &self.role,
                        expected.start_time_secs,
                        self.now_secs(),
                    ) {
                        None => return Ok(()),
                        Some(p) => {
                            last =
                                format!("the lane's claude (distro pid {}) is still running", p.pid)
                        }
                    }
                }
                Err(e) => last = format!("could not confirm the lane is gone: {e}"),
            }
        }
        Err(KillError::SignalRejected(format!(
            "the host's wsl.exe was ended but {last}"
        )))
    }

    /// The lane's `claude` started at or after `after_start_time_secs`, by the probe. The pid
    /// returned is the distro's, not a Windows pid.
    fn find_claude_process_in(&self, _cwd: &str, after_start_time_secs: u64) -> Option<u32> {
        let procs = self.probe().ok()?;
        lane_claude(&procs, &self.role, after_start_time_secs, self.now_secs()).map(|p| p.pid)
    }

    fn process_table(&self) -> Result<Vec<ProcEntry>, ShellCheckError> {
        self.inner.process_table()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    fn target() -> WslTarget {
        WslTarget {
            distro: "lane-spike".to_string(),
            user: "lane-commysdk".to_string(),
            wrapper: "/usr/local/bin/lane-sandbox".to_string(),
            state_dir: None,
        }
    }

    const BWRAP: &str = "bwrap --die-with-parent --new-session --unshare-pid --ro-bind /usr /usr \
                         --bind /home/lane-commysdk /home/lane-commysdk --setenv HOME /home/lane-commysdk \
                         /usr/local/bin/claude read-x-HANDOFF --name commy-sdk-rust --model sonnet";

    /// What the probe printed with the lane up: the user's systemd and shell, two `bwrap` lines
    /// whose tail is the lane's argv (measured), the claude, a tool shell below it, and ps itself.
    fn lane_up() -> String {
        format!(
            "    355     1 900 /usr/lib/systemd/systemd --user\n\
                 381     1 900 -bash\n\
                 403   381  60 {BWRAP}\n\
                 405   403  60 {BWRAP}\n\
                 406   405  59 claude read commy-sdk-rust HANDOFF and continue --name commy-sdk-rust --model sonnet\n\
                 420   406   5 /bin/bash -c cargo test\n\
                 430   420   4 cargo test\n\
                 999   381   0 /bin/ps -u lane-commysdk -o pid=,ppid=,etimes=,args=\n"
        )
    }

    fn lane_gone() -> String {
        "    355     1 960 /usr/lib/systemd/systemd --user\n    381     1 960 -bash\n    999   381   0 /bin/ps -u lane-commysdk\n"
            .to_string()
    }

    /// Answers each call from a queue; the last answer repeats.
    struct Script {
        answers: RefCell<VecDeque<Result<String, String>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Script {
        fn new(answers: Vec<Result<String, String>>) -> Rc<Self> {
            Rc::new(Self {
                answers: RefCell::new(answers.into()),
                calls: RefCell::new(Vec::new()),
            })
        }
    }

    impl DistroRunner for Rc<Script> {
        fn run(&self, argv: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(argv.to_vec());
            let mut q = self.answers.borrow_mut();
            if q.len() > 1 {
                q.pop_front().unwrap()
            } else {
                q.front()
                    .cloned()
                    .unwrap_or_else(|| Err("no answer".into()))
            }
        }
    }

    const NOW: i64 = 1_000_000;
    /// The host's wsl.exe started 100 s before NOW; the lane's claude 59 s before NOW.
    const HOST_START: u64 = 1_000_000 - 100;

    struct Inner {
        host: Option<ProcessIdentity>,
        killed: Cell<u32>,
        kill_result: Result<(), KillError>,
    }

    fn wsl_exe(start: u64) -> ProcessIdentity {
        ProcessIdentity {
            start_time_secs: start,
            exe: Some(PathBuf::from(r"C:\Windows\System32\wsl.exe")),
        }
    }

    impl SystemFacts for Inner {
        fn is_alive_claude_process(&self, _: u32) -> bool {
            unreachable!("WslFacts never asks the Windows side if wsl.exe is claude")
        }
        fn cwd_of(&self, _: u32) -> Option<PathBuf> {
            unreachable!()
        }
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, ShellCheckError> {
            unreachable!()
        }
        fn transcript_is_recent(&self, _: &str, _: &str, _: Duration) -> bool {
            unreachable!()
        }
        fn now(&self) -> DateTime<Utc> {
            Utc.timestamp_opt(NOW, 0).unwrap()
        }
        fn process_identity(&self, _: u32) -> Option<ProcessIdentity> {
            self.host.clone()
        }
        fn kill_verified(&self, _: u32, _: &ProcessIdentity) -> Result<(), KillError> {
            self.killed.set(self.killed.get() + 1);
            match &self.kill_result {
                Ok(()) => Ok(()),
                Err(KillError::IdentityChanged) => Err(KillError::IdentityChanged),
                Err(KillError::NoLongerRunning) => Err(KillError::NoLongerRunning),
                Err(KillError::SignalRejected(m)) => Err(KillError::SignalRejected(m.clone())),
            }
        }
        fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
            unreachable!()
        }
        fn process_table(&self) -> Result<Vec<ProcEntry>, ShellCheckError> {
            Ok(Vec::new())
        }
    }

    fn inner(host: Option<ProcessIdentity>) -> Inner {
        Inner {
            host,
            killed: Cell::new(0),
            kill_result: Ok(()),
        }
    }

    fn facts(
        host: Option<ProcessIdentity>,
        answers: Vec<Result<String, String>>,
    ) -> (WslFacts<Inner, Rc<Script>>, Rc<Script>) {
        let script = Script::new(answers);
        let f = WslFacts::new(
            inner(host),
            Rc::clone(&script),
            target(),
            "commy-sdk-rust",
            "/home/lane-commysdk/work/commy-sdk-rust",
            PathBuf::from("."),
        )
        .with_sleep(Box::new(|_| {}), 3, Duration::from_millis(1));
        (f, script)
    }

    // ---- the fixed command ----

    #[test]
    fn the_probe_is_one_fixed_argv_with_no_shell_and_only_the_checked_names() {
        assert_eq!(
            ps_argv(&target()),
            [
                "wsl.exe",
                "-d",
                "lane-spike",
                "-u",
                "lane-commysdk",
                "--exec",
                "/bin/ps",
                "-u",
                "lane-commysdk",
                "-o",
                "pid=,ppid=,etimes=,args="
            ]
        );
    }

    // ---- reading the output ----

    #[test]
    fn the_probe_output_parses_to_rows_and_keeps_each_command_line() {
        let procs = parse_ps(&lane_up()).unwrap();
        assert_eq!(procs.len(), 8);
        assert_eq!(procs[0].pid, 355);
        assert_eq!(procs[0].ppid, 1);
        assert_eq!(procs[0].elapsed_secs, 900);
        assert_eq!(procs[0].args, ["/usr/lib/systemd/systemd", "--user"]);
        assert_eq!(procs[2].args[0], "bwrap");
    }

    #[test]
    fn a_line_that_is_not_a_ps_row_or_an_empty_answer_is_an_error_not_a_smaller_table() {
        for bad in [
            "",
            "   \n\n",
            "abc 1 2 x",
            "1 2",
            "1 2 3",
            "1 x 3 cmd",
            "355 1 900 systemd\nthis is not a row\n",
            "99999999999 1 2 cmd",
        ] {
            assert!(parse_ps(bad).is_err(), "{bad:?} parsed");
        }
    }

    // ---- which process is the lane ----

    #[test]
    fn the_lane_is_the_claude_not_the_bwrap_lines_that_carry_its_argv() {
        let procs = parse_ps(&lane_up()).unwrap();
        let p = lane_claude(&procs, "commy-sdk-rust", HOST_START, NOW as u64).unwrap();
        assert_eq!(p.pid, 406);
        // control: the same query without the program rule would also take the bwrap lines
        let loose = procs
            .iter()
            .filter(|p| {
                p.args
                    .windows(2)
                    .any(|w| w[0] == "--name" && w[1] == "commy-sdk-rust")
            })
            .count();
        assert_eq!(loose, 3, "two bwrap lines and the claude all carry --name");
    }

    #[test]
    fn a_wrapper_left_behind_after_claude_exited_is_not_a_live_lane() {
        // `bwrap` carries the lane's argv; with the claude gone it is all that is left to match.
        let only_wrapper = format!(
            "381 1 900 -bash
403 381 60 {BWRAP}
405 403 60 {BWRAP}
999 381 0 /bin/ps -u u
"
        );
        let procs = parse_ps(&only_wrapper).unwrap();
        assert!(lane_claude(&procs, "commy-sdk-rust", HOST_START, NOW as u64).is_none());
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(only_wrapper)]);
        assert!(!f.is_alive_claude_process(4242));
        assert_eq!(f.find_claude_process_in("/x", 0), None);
    }

    #[test]
    fn a_claude_started_by_its_full_path_is_the_lane_and_a_sibling_lane_is_not() {
        let out = "10 1 50 /home/lane-commysdk/.local/bin/claude hi --name commy-sdk-rust\n\
                   11 1 50 claude hi --name other-lane\n\
                   12 1 50 claudex hi --name commy-sdk-rust\n";
        let procs = parse_ps(out).unwrap();
        assert_eq!(
            lane_claude(&procs, "commy-sdk-rust", 0, NOW as u64).map(|p| p.pid),
            Some(10)
        );
        assert_eq!(
            lane_claude(&procs, "other-lane", 0, NOW as u64).map(|p| p.pid),
            Some(11)
        );
        assert!(lane_claude(&procs, "absent", 0, NOW as u64).is_none());
        // `--name` must be followed by exactly the role, not merely contain it
        let prefix = parse_ps("13 1 5 claude --name commy-sdk-rust-2\n").unwrap();
        assert!(lane_claude(&prefix, "commy-sdk-rust", 0, NOW as u64).is_none());
    }

    #[test]
    fn a_claude_that_started_before_the_host_is_an_older_session_not_this_lane() {
        let procs = parse_ps("20 1 5000 claude hi --name commy-sdk-rust\n").unwrap();
        // started 5000 s ago; the host started 100 s ago
        assert!(lane_claude(&procs, "commy-sdk-rust", HOST_START, NOW as u64).is_none());
        // the same row is the lane for a host that started 6000 s ago
        assert!(lane_claude(&procs, "commy-sdk-rust", NOW as u64 - 6000, NOW as u64).is_some());
    }

    #[test]
    fn two_claudes_of_one_role_pick_the_newest() {
        let procs = parse_ps("30 1 90 claude a --name r\n31 1 20 claude a --name r\n").unwrap();
        assert_eq!(lane_claude(&procs, "r", 0, NOW as u64).unwrap().pid, 31);
    }

    #[test]
    fn a_shell_below_the_lane_is_found_through_its_children_and_a_sibling_shell_is_not() {
        let procs = parse_ps(&lane_up()).unwrap();
        assert!(has_shell_descendant(&procs, 406), "bash under claude");
        assert!(
            !has_shell_descendant(&procs, 430),
            "cargo has no children: control"
        );
        let no_shell = parse_ps(
            "406 405 59 claude a --name r\n430 406 4 cargo test\n431 430 3 rustc x\n381 1 90 -bash\n",
        )
        .unwrap();
        assert!(
            !has_shell_descendant(&no_shell, 406),
            "the login -bash is not below claude"
        );
        let deep = parse_ps("1 0 9 claude a\n2 1 9 node x\n3 2 9 /usr/bin/zsh -c y\n").unwrap();
        assert!(has_shell_descendant(&deep, 1), "a shell two levels down");
        let cycle = parse_ps("1 2 9 claude a\n2 1 9 node x\n").unwrap();
        assert!(!has_shell_descendant(&cycle, 1), "a ppid cycle ends");
    }

    #[test]
    fn the_image_name_is_the_last_component_of_either_kind_of_path_on_every_os() {
        for (path, want) in [
            (r"C:\Windows\System32\wsl.exe", "wsl.exe"),
            ("C:/Windows/System32/WSL.EXE", "WSL.EXE"),
            ("/usr/bin/wsl.exe", "wsl.exe"),
            ("wsl.exe", "wsl.exe"),
            (r"C:\Windows\System32\wsl.exe.bak", "wsl.exe.bak"),
        ] {
            assert_eq!(image_name(std::path::Path::new(path)), want, "{path}");
        }
    }

    // ---- the facts ----

    #[test]
    fn alive_needs_both_the_hosts_wsl_exe_and_the_lane_claude() {
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_up())]);
        assert!(f.is_alive_claude_process(4242));
        // host gone
        let (f, s) = facts(None, vec![Ok(lane_up())]);
        assert!(!f.is_alive_claude_process(4242));
        assert!(s.calls.borrow().is_empty(), "no point asking the distro");
        // claude gone
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_gone())]);
        assert!(!f.is_alive_claude_process(4242));
        // the probe failed: not alive, never a guess
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Err("wsl.exe: boom".into())]);
        assert!(!f.is_alive_claude_process(4242));
    }

    #[test]
    fn a_pid_whose_image_is_not_wsl_exe_is_not_the_host() {
        for exe in [Some(PathBuf::from(r"C:\Windows\notepad.exe")), None] {
            let id = ProcessIdentity {
                start_time_secs: HOST_START,
                exe,
            };
            let (f, _) = facts(Some(id), vec![Ok(lane_up())]);
            assert!(!f.is_alive_claude_process(4242));
            assert!(f.process_identity(4242).is_none());
            assert!(f.cwd_of(4242).is_none());
        }
    }

    #[test]
    fn the_cwd_is_the_recorded_posix_launch_directory() {
        let (f, s) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_up())]);
        assert_eq!(
            f.cwd_of(4242),
            Some(PathBuf::from("/home/lane-commysdk/work/commy-sdk-rust"))
        );
        assert!(
            s.calls.borrow().is_empty(),
            "read from the record, not the distro"
        );
    }

    #[test]
    fn a_live_shell_below_the_lane_blocks_and_a_failed_or_blind_probe_blocks_too() {
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_up())]);
        assert_eq!(f.has_live_shell_descendant(4242), Ok(true));
        let quiet = "406 405 59 claude a --name commy-sdk-rust\n430 406 4 cargo test\n".to_string();
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(quiet)]);
        assert_eq!(f.has_live_shell_descendant(4242), Ok(false));
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Err("boom".into())]);
        assert!(matches!(
            f.has_live_shell_descendant(4242),
            Err(ShellCheckError::EnumerationFailed(_))
        ));
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_gone())]);
        assert!(
            matches!(
                f.has_live_shell_descendant(4242),
                Err(ShellCheckError::EnumerationFailed(_))
            ),
            "no claude found is not 'no shell'"
        );
        let (f, _) = facts(None, vec![Ok(lane_up())]);
        assert!(matches!(
            f.has_live_shell_descendant(4242),
            Err(ShellCheckError::EnumerationFailed(_))
        ));
    }

    #[test]
    fn a_kill_is_confirmed_only_when_the_probe_shows_the_lane_gone() {
        let (f, s) = facts(
            Some(wsl_exe(HOST_START)),
            vec![Ok(lane_up()), Ok(lane_gone())],
        );
        assert_eq!(f.kill_verified(4242, &wsl_exe(HOST_START)), Ok(()));
        assert_eq!(f.inner.killed.get(), 1);
        assert_eq!(
            s.calls.borrow().len(),
            2,
            "one survivor sighting, then gone"
        );
    }

    #[test]
    fn a_survivor_after_the_kill_is_reported_and_names_the_distro_pid() {
        let (f, s) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_up())]);
        let err = f.kill_verified(4242, &wsl_exe(HOST_START)).unwrap_err();
        match err {
            KillError::SignalRejected(m) => {
                assert!(m.contains("406"), "{m}");
                assert!(m.contains("still running"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.calls.borrow().len(), 3, "the confirm attempts, no more");
    }

    #[test]
    fn a_kill_that_cannot_be_confirmed_is_not_a_success() {
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Err("wsl.exe: boom".into())]);
        let err = f.kill_verified(4242, &wsl_exe(HOST_START)).unwrap_err();
        assert!(
            matches!(&err, KillError::SignalRejected(m) if m.contains("could not confirm")),
            "{err:?}"
        );
    }

    #[test]
    fn a_refused_windows_kill_is_returned_and_the_distro_is_never_asked() {
        let script = Script::new(vec![Ok(lane_gone())]);
        let mut i = inner(Some(wsl_exe(HOST_START)));
        i.kill_result = Err(KillError::IdentityChanged);
        let f = WslFacts::new(
            i,
            Rc::clone(&script),
            target(),
            "commy-sdk-rust",
            "/x",
            PathBuf::from("."),
        );
        assert_eq!(
            f.kill_verified(4242, &wsl_exe(HOST_START)),
            Err(KillError::IdentityChanged)
        );
        assert!(script.calls.borrow().is_empty());
    }

    #[test]
    fn a_new_session_is_found_by_the_probe_after_the_moment_of_the_kill() {
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Ok(lane_up())]);
        assert_eq!(f.find_claude_process_in("/x", HOST_START), Some(406));
        // started 59 s before NOW, so it is not after a moment 10 s before NOW
        assert_eq!(f.find_claude_process_in("/x", NOW as u64 - 10), None);
        let (f, _) = facts(Some(wsl_exe(HOST_START)), vec![Err("boom".into())]);
        assert_eq!(f.find_claude_process_in("/x", 0), None);
    }

    #[test]
    fn the_transcript_is_read_where_windows_can_see_the_distros_files() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = "/home/lane-commysdk/work/commy-sdk-rust";
        let project = dir
            .path()
            .join("projects")
            .join(lane_state::paths::project_dir_name(cwd));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("sess-1.jsonl"), "{}").unwrap();
        let script = Script::new(vec![Ok(lane_up())]);
        let f = WslFacts::new(
            inner(Some(wsl_exe(HOST_START))),
            Rc::clone(&script),
            target(),
            "commy-sdk-rust",
            cwd,
            dir.path().to_path_buf(),
        );
        assert!(f.transcript_is_recent(cwd, "sess-1", Duration::from_secs(60)));
        assert!(
            !f.transcript_is_recent(cwd, "sess-2", Duration::from_secs(60)),
            "missing"
        );
        assert!(
            !f.transcript_is_recent("/home/other", "sess-1", Duration::from_secs(60)),
            "other project"
        );
        assert!(script.calls.borrow().is_empty());
    }
}
