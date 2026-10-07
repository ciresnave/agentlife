// SPDX-License-Identifier: MIT OR Apache-2.0
//! A create-new-file lock around one read-modify-write cycle.
//!
//! The same shape as `lane-restart`'s `StateLock` (`lane_state_writer.rs`, OverMind `3bf513f`):
//! the lock is a file created with `create_new` (atomic at the OS level), a lock older than
//! `stale_after` is treated as abandoned and reclaimed so a crashed holder cannot wedge every
//! later writer, and a waiter gives up only after `timeout`.
//!
//! Three lessons from that code are built in and tested here:
//!
//! 1. **`timeout` must exceed `stale_after`.** OverMind's first constants gave up at 2 s, before
//!    anyone was *allowed* to reclaim a 5 s-old lock, so a dead holder blocked a legitimate
//!    writer. [`DEFAULT_ACQUIRE_TIMEOUT`] is asserted greater than [`DEFAULT_STALE_AFTER`].
//! 2. **On Windows a `create_new` racing a `remove_file` reports `PermissionDenied`**, not
//!    `AlreadyExists`. Both mean "busy, retry".
//! 3. **A holder must not delete a lock it no longer owns.** Each lock file holds a unique token,
//!    and `Drop` removes the file only if the token is still its own, so a holder whose lock was
//!    reclaimed as stale cannot delete its successor's.
//!
//! A holder keeps the lock for one short read-modify-write, never across a long operation: a
//! lock held longer than `stale_after` can legitimately be reclaimed from under it.

use std::fmt;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(7);
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(2);

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum LockError {
    TimedOut(PathBuf),
    Io(std::io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::TimedOut(p) => write!(f, "timed out waiting for the lock {}", p.display()),
            LockError::Io(e) => write!(f, "lock I/O error: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// A held lock. Dropping it releases it.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    token: String,
}

fn new_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}.{nanos}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn is_busy(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::AlreadyExists | ErrorKind::PermissionDenied)
}

fn is_stale(path: &Path, stale_after: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > stale_after)
}

impl FileLock {
    pub fn acquire(
        path: impl Into<PathBuf>,
        timeout: Duration,
        stale_after: Duration,
    ) -> Result<Self, LockError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LockError::Io)?;
        }
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let token = new_token();
                    // Best effort: a lock whose token could not be written is simply never
                    // deleted by `Drop` and is reclaimed as stale.
                    let _ = f.write_all(token.as_bytes());
                    return Ok(Self { path, token });
                }
                Err(e) if is_busy(e.kind()) => {
                    if is_stale(&path, stale_after) {
                        // Rename before removing: of several reclaimers only one rename wins.
                        let graveyard = path.with_extension(format!(
                            "stale.{}.{}",
                            std::process::id(),
                            COUNTER.fetch_add(1, Ordering::Relaxed)
                        ));
                        if std::fs::rename(&path, &graveyard).is_ok() {
                            let _ = std::fs::remove_file(&graveyard);
                        }
                        continue;
                    }
                    if start.elapsed() >= timeout {
                        return Err(LockError::TimedOut(path));
                    }
                    std::thread::sleep(POLL);
                }
                Err(e) => return Err(LockError::Io(e)),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|content| content == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn backdate(path: &Path, by: Duration) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn the_production_constants_let_a_waiter_reclaim_before_giving_up() {
        // The OverMind bug: timeout 2 s < stale 5 s meant nobody could ever reclaim in time.
        assert!(DEFAULT_ACQUIRE_TIMEOUT > DEFAULT_STALE_AFTER);
    }

    #[test]
    fn acquire_then_drop_removes_the_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let l = FileLock::acquire(&p, Duration::from_secs(1), Duration::from_secs(5)).unwrap();
        assert!(p.exists());
        drop(l);
        assert!(!p.exists());
    }

    #[test]
    fn a_second_acquire_waits_for_the_first_to_drop() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let first = FileLock::acquire(&p, Duration::from_secs(1), Duration::from_secs(30)).unwrap();
        let p2 = p.clone();
        let waiter = std::thread::spawn(move || {
            let t = Instant::now();
            let _l =
                FileLock::acquire(&p2, Duration::from_secs(5), Duration::from_secs(30)).unwrap();
            t.elapsed()
        });
        std::thread::sleep(Duration::from_millis(150));
        drop(first);
        let waited = waiter.join().unwrap();
        assert!(
            waited >= Duration::from_millis(100),
            "the waiter should have blocked, waited {waited:?}"
        );
    }

    #[test]
    fn it_times_out_rather_than_blocking_forever() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let _held = FileLock::acquire(&p, Duration::from_secs(1), Duration::from_secs(30)).unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(60), Duration::from_secs(30));
        assert!(matches!(r, Err(LockError::TimedOut(_))), "got {r:?}");
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, "dead-holder").unwrap();
        backdate(&p, Duration::from_secs(60));
        let l = FileLock::acquire(&p, Duration::from_millis(500), Duration::from_secs(5));
        assert!(l.is_ok(), "a 60 s old lock must be reclaimed: {l:?}");
    }

    #[test]
    fn a_fresh_lock_is_not_reclaimed() {
        // Positive control for the test above: the same lock, not backdated, is respected.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, "live-holder").unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(60), Duration::from_secs(5));
        assert!(matches!(r, Err(LockError::TimedOut(_))), "got {r:?}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "live-holder");
    }

    #[test]
    fn a_holder_whose_lock_was_reclaimed_does_not_delete_its_successors() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let first = FileLock::acquire(&p, Duration::from_secs(1), Duration::from_secs(5)).unwrap();
        backdate(&p, Duration::from_secs(60));
        let second =
            FileLock::acquire(&p, Duration::from_millis(500), Duration::from_secs(5)).unwrap();
        drop(first);
        assert!(
            p.exists(),
            "the first holder's drop must not delete the second's lock"
        );
        drop(second);
        assert!(!p.exists());
    }

    #[test]
    fn twenty_real_threads_never_lose_an_update() {
        // The proof the lock actually excludes: a deliberately non-atomic read-modify-write
        // on one file, 20 threads x 25 increments, must total 500.
        let d = tempfile::tempdir().unwrap();
        let lock = d.path().join("c.lock");
        let counter = d.path().join("c.txt");
        std::fs::write(&counter, "0").unwrap();
        let start = Arc::new(Barrier::new(20));
        let handles: Vec<_> = (0..20)
            .map(|_| {
                let (lock, counter, start) = (lock.clone(), counter.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..25 {
                        let _g = FileLock::acquire(
                            &lock,
                            Duration::from_secs(30),
                            Duration::from_secs(20),
                        )
                        .unwrap();
                        let n: u64 = std::fs::read_to_string(&counter)
                            .unwrap()
                            .trim()
                            .parse()
                            .unwrap();
                        std::fs::write(&counter, (n + 1).to_string()).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let total: u64 = std::fs::read_to_string(&counter)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(total, 500, "lost updates under contention");
    }
}
