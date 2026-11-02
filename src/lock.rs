// SPDX-License-Identifier: MIT OR Apache-2.0
//! A create-new-file lock around one read-modify-write cycle.
//!
//! The same shape as `lane-restart`'s `StateLock` (`lane_state_writer.rs`, OverMind `3bf513f`):
//! the lock is a file created with `create_new` (atomic at the OS level), a lock older than
//! `stale_after` is treated as abandoned and reclaimed so a crashed holder cannot wedge every
//! later writer, and a waiter gives up only after `timeout`.
//!
//! Four lessons from that code are built in and tested here:
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
//! 4. **A lock is abandoned only if it is old AND its owner is gone** (issue #241). Each token
//!    names its owner as `pid:start:...`; an old lock whose owner process is still running (same
//!    pid, same start time) is NOT reclaimed, however long the holder has been slow. A loaded
//!    machine held one registry lock for 7 s, a waiter robbed it at 5 s, and two writers
//!    read-modify-wrote at once: an `Ok` update silently lost. Now such a waiter waits and, past
//!    its `timeout`, fails with [`LockError::TimedOut`]: loud, not lossy. (Start times within a
//!    second count as equal: see `same_start`.) An unparsable token (an
//!    empty file, or one written by an older binary) is judged by age alone, as before.
//!
//! A holder keeps the lock for one short read-modify-write, never across a long operation: a
//! lock whose owner has died can be reclaimed once it is `stale_after` old.

use std::fmt;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use crate::identity::{ProcessTable, SysinfoTable};

pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(7);
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(2);
/// How often a waiter that finds an old lock re-asks whether its owner is still running. Reading
/// the process table is far slower than a poll, so the answer is not asked for every 2 ms.
const OWNER_RECHECK: Duration = Duration::from_millis(250);

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

/// This process's start time (Unix seconds), read once; `None` when the process table cannot be
/// read, in which case tokens carry no owner and are judged by age alone.
fn own_start_secs() -> Option<u64> {
    static START: OnceLock<Option<u64>> = OnceLock::new();
    *START.get_or_init(|| {
        SysinfoTable
            .identity_of(std::process::id())
            .map(|i| i.start_secs)
    })
}

fn new_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    match own_start_secs() {
        Some(start) => format!("{}:{start}:{nanos}.{counter}", std::process::id()),
        None => format!("{}.{nanos}.{counter}", std::process::id()),
    }
}

/// The owner a token names: `pid:start:...`. `None` for any other shape.
fn parse_owner(token: &str) -> Option<(u32, u64)> {
    let mut parts = token.splitn(3, ':');
    let pid = parts.next()?.parse().ok()?;
    let start = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((pid, start))
}

/// Do two readings of a start time name the same process? Within a second counts as the same: off
/// Windows `sysinfo` derives a start time from the boot time, which can move by a second between two
/// reads of one live process (issue #14), and here a false "different process" would rob a live
/// holder, the very loss this module exists to prevent. A recycled pid started within a second of
/// the dead owner's own start is not a case worth waiting for.
fn same_start(recorded: u64, now: u64) -> bool {
    recorded.abs_diff(now) <= 1
}

/// Is the process that took the lock at `path` gone? `true` when the token names no owner
/// (nothing to wait for: age decides, as it always did), when nothing runs under that pid, or
/// when what runs there started at a different time (a recycled pid).
fn owner_is_gone(path: &Path) -> bool {
    let Ok(token) = std::fs::read_to_string(path) else {
        return true;
    };
    let Some((pid, start)) = parse_owner(&token) else {
        return true;
    };
    match SysinfoTable.identity_of(pid) {
        None => true,
        Some(now) => !same_start(start, now.start_secs),
    }
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
        // When an old lock's owner was last found still running, and when to ask again.
        let mut owner_alive_until: Option<Instant> = None;
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
                        let due = owner_alive_until.is_none_or(|t| Instant::now() >= t);
                        if due {
                            if owner_is_gone(&path) {
                                // Rename before removing: of several reclaimers only one rename
                                // wins.
                                let graveyard = path.with_extension(format!(
                                    "stale.{}.{}",
                                    std::process::id(),
                                    COUNTER.fetch_add(1, Ordering::Relaxed)
                                ));
                                if std::fs::rename(&path, &graveyard).is_ok() {
                                    let _ = std::fs::remove_file(&graveyard);
                                }
                                owner_alive_until = None;
                                continue;
                            }
                            // Old, but its owner is running: slow, not dead. Wait for it.
                            owner_alive_until = Some(Instant::now() + OWNER_RECHECK);
                        }
                    } else {
                        owner_alive_until = None;
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
        // The first holder's lock comes to look abandoned: old, and owned by a process that is
        // gone (a live owner's lock is no longer reclaimed, however old; see issue #241).
        std::fs::write(&p, "4294967290:1:0.0").unwrap();
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

    /// Issue #241: the lock is judged abandoned by its file's age alone, so a LIVE holder that is
    /// slow (a loaded machine held one lock for 7 s) loses it to the next waiter and both run their
    /// read-modify-write at once. Here the holder is slower than `stale_after` on purpose; the
    /// waiter must still wait for it.
    #[test]
    fn a_live_holder_slower_than_stale_after_is_not_robbed_of_its_lock() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let inside = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let overlaps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let enter = |inside: &std::sync::atomic::AtomicUsize,
                     overlaps: &std::sync::atomic::AtomicUsize| {
            if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                overlaps.fetch_add(1, Ordering::SeqCst);
            }
        };
        let holder =
            FileLock::acquire(&p, Duration::from_secs(1), Duration::from_millis(100)).unwrap();
        enter(&inside, &overlaps);
        let waiter = {
            let (p, inside, overlaps) = (p.clone(), inside.clone(), overlaps.clone());
            std::thread::spawn(move || {
                let _l = FileLock::acquire(&p, Duration::from_secs(5), Duration::from_millis(100))
                    .unwrap();
                if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                    overlaps.fetch_add(1, Ordering::SeqCst);
                }
                inside.fetch_sub(1, Ordering::SeqCst);
            })
        };
        // Four times `stale_after`, with the holder alive and inside its critical section.
        std::thread::sleep(Duration::from_millis(400));
        inside.fetch_sub(1, Ordering::SeqCst);
        drop(holder);
        waiter.join().unwrap();
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "the waiter entered while the live holder was still inside"
        );
    }

    /// The token names its owner as `pid:start:...`.
    fn token_of(pid: u32, start: u64) -> String {
        format!("{pid}:{start}:0.0")
    }

    fn own_start() -> u64 {
        use crate::identity::ProcessTable;
        crate::identity::SysinfoTable
            .identity_of(std::process::id())
            .expect("this process is in the table")
            .start_secs
    }

    #[test]
    fn an_old_lock_whose_owner_process_is_alive_is_not_reclaimed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, token_of(std::process::id(), own_start())).unwrap();
        backdate(&p, Duration::from_secs(60));
        let r = FileLock::acquire(&p, Duration::from_millis(300), Duration::from_secs(5));
        assert!(matches!(r, Err(LockError::TimedOut(_))), "got {r:?}");
    }

    #[test]
    fn an_old_lock_whose_owner_process_is_gone_is_reclaimed() {
        // Positive control for the test above: the same old lock, owner not running.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, token_of(4_294_967_290, 1)).unwrap();
        backdate(&p, Duration::from_secs(60));
        let r = FileLock::acquire(&p, Duration::from_millis(500), Duration::from_secs(5));
        assert!(
            r.is_ok(),
            "a dead owner's old lock must be reclaimed: {r:?}"
        );
    }

    #[test]
    fn a_start_time_one_second_off_is_the_same_process_and_two_seconds_off_is_not() {
        assert!(same_start(1000, 1000));
        assert!(same_start(1000, 1001));
        assert!(same_start(1001, 1000));
        assert!(!same_start(1000, 1002));
        assert!(!same_start(1002, 1000));
    }

    #[test]
    fn an_old_lock_whose_owner_start_reads_one_second_off_is_still_not_reclaimed() {
        // The owner is alive; its start time reads a second differently than when it took the lock.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, token_of(std::process::id(), own_start() + 1)).unwrap();
        backdate(&p, Duration::from_secs(60));
        let r = FileLock::acquire(&p, Duration::from_millis(300), Duration::from_secs(5));
        assert!(matches!(r, Err(LockError::TimedOut(_))), "got {r:?}");
    }

    #[test]
    fn an_old_lock_whose_pid_now_belongs_to_a_different_process_is_reclaimed() {
        // The pid is alive (this process) but the recorded start time is another process's.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, token_of(std::process::id(), own_start() + 1000)).unwrap();
        backdate(&p, Duration::from_secs(60));
        let r = FileLock::acquire(&p, Duration::from_millis(500), Duration::from_secs(5));
        assert!(r.is_ok(), "a recycled pid's lock must be reclaimed: {r:?}");
    }

    #[test]
    fn a_fresh_lock_of_a_dead_owner_is_still_waited_for_until_it_is_old() {
        // Age is still required: a dead owner's lock younger than `stale_after` is respected.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        std::fs::write(&p, token_of(4_294_967_290, 1)).unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(100), Duration::from_secs(5));
        assert!(matches!(r, Err(LockError::TimedOut(_))), "got {r:?}");
    }

    #[test]
    fn a_new_lock_records_its_owner_and_the_owner_is_this_process() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.lock");
        let l = FileLock::acquire(&p, Duration::from_secs(1), Duration::from_secs(5)).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let mut parts = body.splitn(3, ':');
        assert_eq!(
            parts.next().unwrap().parse::<u32>().unwrap(),
            std::process::id()
        );
        assert_eq!(parts.next().unwrap().parse::<u64>().unwrap(), own_start());
        drop(l);
    }
}
