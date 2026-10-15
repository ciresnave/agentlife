// SPDX-License-Identifier: MIT OR Apache-2.0

//! The per-role start lock (`docs/LAUNCHER-SERVICE.md`, "Two starters at once").
//!
//! `plan_start` reads the process table and `execute_start` spawns later, with nothing between
//! them: a launcher request racing a person's `lane-restart start`, or two launches of one role,
//! could both pass the check. `<state>/<role>.start.lock` is an OS advisory lock held by the one
//! starter for the whole of check, spawn and the wait for the session to be confirmed live. The
//! OS releases it when the holder exits or crashes, so there is no stale file to reclaim.
//!
//! The check runs *after* the lock is taken (the caller plans under the guard), so the second
//! starter sees the first one's session, or is refused the lock and told `already-running`.

use crate::relaunch::valid_identifier;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// Why the lock was not taken.
#[derive(Debug)]
pub enum StartLockError {
    /// Another starter holds it: the role is being started right now.
    Held,
    /// The lock file could not be created or locked (permissions, a missing state dir).
    Unavailable(String),
    /// `role` is not a plain identifier, so it must not become part of a path.
    InvalidRole,
}

/// The lock file's name for `role`. Not `*.json`, so no reader of the state dir mistakes it for a
/// lane's state file.
pub fn start_lock_path(dir: &Path, role: &str) -> PathBuf {
    dir.join(format!("{role}.start.lock"))
}

/// Held while a start of one role is in flight; dropping it releases the lock.
#[derive(Debug)]
pub struct StartLock {
    _file: File,
}

/// Takes `<dir>/<role>.start.lock` without waiting. Create-or-open of the same path, so there is
/// nothing to race on; the file's content is never read.
pub fn acquire(dir: &Path, role: &str) -> Result<StartLock, StartLockError> {
    if !valid_identifier(role) {
        return Err(StartLockError::InvalidRole);
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(start_lock_path(dir, role))
        .map_err(|e| StartLockError::Unavailable(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => Ok(StartLock { _file: file }),
        Err(TryLockError::WouldBlock) => Err(StartLockError::Held),
        Err(TryLockError::Error(e)) => Err(StartLockError::Unavailable(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_second_starter_of_the_same_role_is_refused_while_the_first_holds_it() {
        let d = tempdir().unwrap();
        let first = acquire(d.path(), "lightbulb").unwrap();
        assert!(matches!(
            acquire(d.path(), "lightbulb"),
            Err(StartLockError::Held)
        ));
        drop(first);
        assert!(acquire(d.path(), "lightbulb").is_ok());
    }

    #[test]
    fn different_roles_do_not_contend() {
        let d = tempdir().unwrap();
        let _a = acquire(d.path(), "lightbulb").unwrap();
        assert!(acquire(d.path(), "coderipper").is_ok());
    }

    #[test]
    fn a_role_that_is_not_an_identifier_never_reaches_a_path() {
        let d = tempdir().unwrap();
        for bad in ["", "../pm", "a/b", "a\\b", "x.y"] {
            assert!(
                matches!(acquire(d.path(), bad), Err(StartLockError::InvalidRole)),
                "{bad:?}"
            );
        }
        // positive control: the guard is not simply refusing everything
        assert!(acquire(d.path(), "agentlife").is_ok());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    const HOLD_ENV: &str = "LANE_RESTART_TEST_HOLD_START_LOCK_DIR";

    /// Not a test on its own: the child half of
    /// `a_lock_held_by_another_process_refuses_this_one_until_that_process_ends`. Without the
    /// variable it does nothing (and passes).
    #[test]
    fn child_holds_the_start_lock_until_stdin_closes() {
        use std::io::{Read, Write};
        let Ok(dir) = std::env::var(HOLD_ENV) else {
            return;
        };
        let _lock = acquire(Path::new(&dir), "lightbulb").expect("child takes the lock");
        println!("LOCKED");
        std::io::stdout().flush().unwrap();
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
    }

    /// Two real OS processes (this test binary re-run on the child test above), not two handles in
    /// one process: the advisory lock is per open file description, and the point is that a second
    /// starter in another process is refused and that the lock goes when its holder does.
    #[test]
    fn a_lock_held_by_another_process_refuses_this_one_until_that_process_ends() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let d = tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "start_lock::tests::child_holds_the_start_lock_until_stdin_closes",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HOLD_ENV, d.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let mut locked = false;
        while out.read_line(&mut line).unwrap() > 0 {
            // libtest prefixes the first line of output with "test <name> ... "
            if line.trim_end().ends_with("LOCKED") {
                locked = true;
                break;
            }
            line.clear();
        }
        assert!(locked, "the child never reported the lock");
        assert!(matches!(
            acquire(d.path(), "lightbulb"),
            Err(StartLockError::Held)
        ));
        // another role is not blocked by the child's lock
        assert!(acquire(d.path(), "coderipper").is_ok());
        drop(child.stdin.take()); // EOF: the child exits and the OS releases the lock
        assert!(child.wait().unwrap().success());
        assert!(acquire(d.path(), "lightbulb").is_ok());
    }

    #[test]
    fn a_missing_state_dir_is_unavailable_not_held() {
        let d = tempdir().unwrap();
        let gone = d.path().join("nope");
        assert!(matches!(
            acquire(&gone, "lightbulb"),
            Err(StartLockError::Unavailable(_))
        ));
    }

    #[test]
    fn the_lock_file_is_not_a_state_or_spec_file() {
        let p = start_lock_path(Path::new("s"), "lightbulb");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "lightbulb.start.lock");
        assert!(name.strip_suffix(".json").is_none());
        assert!(!crate::spec::is_spec_file_name(&name));
    }
}
