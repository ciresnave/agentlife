// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ending a process politely on unix (`docs/PORT-LINUX-PLAN.md`, slice P5).
//!
//! `sysinfo::Process::kill()` sends `SIGKILL` on Linux: no chance to flush, and a lane's shells and
//! helpers (its process group) are left running. Windows has no equivalent split and keeps its one
//! call. Here: `SIGTERM` to the target's process group when the target leads one (a session leader
//! on a pty does), else to the process alone; wait a grace period; and only if the same process is
//! still there, `SIGKILL`.
//!
//! ⚠️ The identity is asked again immediately before the `SIGKILL`: during the grace period the
//! target may have exited and its pid been given to something else, and a kill must never reach
//! that. The caller supplies the question, because what "the same process" means (start time, exe)
//! is its own.

use rustix::process::{getpgid, kill_process, kill_process_group, Pid, Signal};
use std::time::{Duration, Instant};

/// How a termination ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The process (or what replaced its pid) was gone within the grace period: `SIGTERM` was enough.
    Terminated,
    /// It was still there after the grace period and `SIGKILL` was sent.
    Killed,
}

/// What `is_target` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The verified process is still there (alive: a zombie is gone).
    Target,
    /// Nothing runs under that pid, or the process is a zombie.
    Gone,
    /// Something else holds the pid now. Never signalled.
    Other,
}

fn send(pid: Pid, group: bool, sig: Signal) -> std::io::Result<()> {
    let r = if group {
        kill_process_group(pid, sig)
    } else {
        kill_process(pid, sig)
    };
    r.map_err(std::io::Error::from)
}

/// `SIGTERM`, wait up to `grace` polling every `poll`, then `SIGKILL` if `is_target` still says
/// [`Presence::Target`]. The signal goes to the whole process group when `pid` is its leader.
pub fn terminate(
    pid: u32,
    is_target: &dyn Fn() -> Presence,
    grace: Duration,
    poll: Duration,
) -> std::io::Result<Outcome> {
    let target = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| std::io::Error::other(format!("{pid} is not a usable pid")))?;
    // A group is signalled only when the target leads it: a process that merely shares its
    // parent's group must not take the parent's other children with it.
    let group = getpgid(Some(target)).is_ok_and(|g| g == target);
    send(target, group, Signal::TERM)?;
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match is_target() {
            Presence::Gone | Presence::Other => return Ok(Outcome::Terminated),
            Presence::Target => std::thread::sleep(poll),
        }
    }
    match is_target() {
        Presence::Target => {
            send(target, group, Signal::KILL)?;
            Ok(Outcome::Killed)
        }
        Presence::Gone | Presence::Other => Ok(Outcome::Terminated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};

    fn alive(pid: u32) -> bool {
        // signal 0: existence only
        Pid::from_raw(pid as i32).is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
    }

    /// The target as `is_target` sees it: zombies are gone, as in the real predicate.
    fn presence_of(pid: u32) -> Presence {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => Presence::Gone,
            Ok(s)
                if s.rsplit(')')
                    .next()
                    .is_some_and(|r| r.trim_start().starts_with('Z')) =>
            {
                Presence::Gone
            }
            Ok(_) => Presence::Target,
        }
    }

    const GRACE: Duration = Duration::from_millis(1500);
    const POLL: Duration = Duration::from_millis(25);

    #[test]
    fn a_process_that_honours_sigterm_is_not_killed() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let out = terminate(pid, &|| presence_of(pid), GRACE, POLL).unwrap();
        assert_eq!(out, Outcome::Terminated);
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(15), "TERM, not KILL: {status:?}");
    }

    /// Control for the one above: a process that ignores TERM is KILLed after the grace period,
    /// and not before it.
    #[test]
    fn a_process_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        let pid = child.id();
        // let the trap install before the signal arrives
        std::thread::sleep(Duration::from_millis(300));
        let t0 = Instant::now();
        let out = terminate(pid, &|| presence_of(pid), GRACE, POLL).unwrap();
        assert_eq!(out, Outcome::Killed);
        assert!(t0.elapsed() >= GRACE, "killed before the grace ended");
        assert_eq!(child.wait().unwrap().signal(), Some(9));
    }

    /// The whole group goes: a leader's helper (here a `sleep` started by its shell) does not
    /// outlive it.
    #[test]
    fn a_group_leaders_children_end_with_it() {
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 60 & echo $!; wait"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        {
            use std::io::BufRead;
            let out = leader.stdout.take().unwrap();
            std::io::BufReader::new(out).read_line(&mut line).unwrap();
        }
        let helper: u32 = line.trim().parse().expect("the helper's pid");
        assert!(alive(helper), "control: the helper runs before the kill");
        let pid = leader.id();
        terminate(pid, &|| presence_of(pid), GRACE, POLL).unwrap();
        let _ = leader.wait();
        // the helper is reparented and reaped by init: poll briefly for it to vanish
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && presence_of(helper) == Presence::Target {
            std::thread::sleep(POLL);
        }
        assert_ne!(
            presence_of(helper),
            Presence::Target,
            "the group's helper survived"
        );
    }

    /// The pid is never signalled once something else holds it.
    #[test]
    fn a_pid_that_changed_hands_during_the_grace_period_is_not_killed() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(300));
        let out = terminate(pid, &|| Presence::Other, GRACE, POLL).unwrap();
        assert_eq!(out, Outcome::Terminated);
        // it ignored TERM and was never KILLed: still running
        assert_eq!(presence_of(pid), Presence::Target);
        child.kill().unwrap();
        let _ = child.wait();
    }
}
