// SPDX-License-Identifier: MIT OR Apache-2.0
//! The append-only journal (DESIGN-REVISION-1 §2.3, DESIGN-REVISION-2 §5.1).
//!
//! One JSON object per line, in **monthly segments** named `journal-YYYY-MM.jsonl` (UTC). The
//! segments are explicit and named, never silently rotated, so the discipline of
//! `lane-restart`'s `restart.log` holds: nothing is overwritten, nothing is deleted, and this
//! module has **no delete or rewrite API at all**.
//!
//! A crash can leave a torn last line. A reader counts such lines instead of failing, and an
//! append that finds a file not ending in a newline first terminates the torn line, so the new
//! entry is never glued onto it and lost.

use crate::clock::Clock;
use crate::lock::{FileLock, LockError, DEFAULT_ACQUIRE_TIMEOUT, DEFAULT_STALE_AFTER};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub at: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub enum JournalError {
    BadKind(String),
    Lock(LockError),
    Io(std::io::Error),
    Encode(serde_json::Error),
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::BadKind(k) => write!(f, "invalid journal kind {k:?}"),
            JournalError::Lock(e) => write!(f, "{e}"),
            JournalError::Io(e) => write!(f, "journal I/O error: {e}"),
            JournalError::Encode(e) => write!(f, "journal encode error: {e}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<std::io::Error> for JournalError {
    fn from(e: std::io::Error) -> Self {
        JournalError::Io(e)
    }
}

/// What a read found: every parsed entry, and how many lines could not be parsed.
#[derive(Debug, Default, PartialEq)]
pub struct ReadResult {
    pub entries: Vec<Entry>,
    pub malformed: usize,
}

pub struct Journal {
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    lock_timeout: Duration,
}

fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// The segment file a moment belongs to.
pub fn segment_name(at: DateTime<Utc>) -> String {
    format!("journal-{:04}-{:02}.jsonl", at.year(), at.month())
}

fn is_segment_name(name: &str) -> bool {
    name.len() == "journal-YYYY-MM.jsonl".len()
        && name.starts_with("journal-")
        && name.ends_with(".jsonl")
        && name["journal-".len()..name.len() - ".jsonl".len()]
            .bytes()
            .enumerate()
            .all(|(i, b)| {
                if i == 4 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
}

impl Journal {
    pub fn new(dir: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self {
            dir: dir.into(),
            clock,
            lock_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    /// How long an append waits for the journal lock. The default suits real use, where appends
    /// are a few per session. A stress test that serialises hundreds of appends on a slow disk
    /// needs longer: that tests "nothing is lost or torn", not "the wait is short".
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one entry stamped with the clock's current time and returns it.
    pub fn append(
        &self,
        kind: &str,
        agent_id: Option<&str>,
        data: serde_json::Value,
    ) -> Result<Entry, JournalError> {
        if !valid_kind(kind) {
            return Err(JournalError::BadKind(kind.to_string()));
        }
        std::fs::create_dir_all(&self.dir)?;
        let entry = Entry {
            at: self.clock.now(),
            kind: kind.to_string(),
            agent_id: agent_id.map(str::to_string),
            data,
        };
        // Compact JSON never contains a raw newline, so one entry is exactly one line.
        let mut line = serde_json::to_vec(&entry).map_err(JournalError::Encode)?;
        line.push(b'\n');

        let lock = FileLock::acquire(
            self.dir.join("journal.lock"),
            self.lock_timeout,
            DEFAULT_STALE_AFTER,
        )
        .map_err(JournalError::Lock)?;
        let path = self.dir.join(segment_name(entry.at));
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        let len = f.metadata()?.len();
        if len > 0 {
            f.seek(SeekFrom::Start(len - 1))?;
            let mut last = [0u8; 1];
            f.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // A torn tail from a crash: end it so this entry starts on its own line.
                f.write_all(b"\n")?;
            }
        }
        f.write_all(&line)?;
        // Release the lock BEFORE the fsync. The line is already written, so the next appender can
        // go ahead, and one fsync covers every write before it. Holding the lock across it made
        // 200 serialised appends wait on 200 disk flushes: on a slow CI disk that exceeded the
        // lock timeout (a real failure, `Lock(TimedOut)`, in the 20-thread test).
        drop(lock);
        f.sync_data()?;
        Ok(entry)
    }

    /// Every segment, oldest first.
    pub fn segments(&self) -> Result<Vec<PathBuf>, JournalError> {
        let mut out = Vec::new();
        match std::fs::read_dir(&self.dir) {
            Ok(rd) => {
                for e in rd {
                    let e = e?;
                    if is_segment_name(&e.file_name().to_string_lossy()) {
                        out.push(e.path());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        out.sort();
        Ok(out)
    }

    /// Reads every segment in order. Unparseable lines are counted, never fatal.
    pub fn read_all(&self) -> Result<ReadResult, JournalError> {
        let mut result = ReadResult::default();
        for seg in self.segments()? {
            let text = String::from_utf8_lossy(&std::fs::read(&seg)?).into_owned();
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<Entry>(line) {
                    Ok(e) => result.entries.push(e),
                    Err(_) => result.malformed += 1,
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{ManualClock, SystemClock};
    use chrono::TimeZone;
    use serde_json::json;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    #[test]
    fn segment_names_are_by_utc_month() {
        assert_eq!(segment_name(at(2026, 10, 7, 1, 0)), "journal-2026-10.jsonl");
        assert_eq!(
            segment_name(at(2026, 1, 31, 23, 59)),
            "journal-2026-01.jsonl"
        );
        assert!(is_segment_name("journal-2026-10.jsonl"));
        assert!(!is_segment_name("journal.lock"));
        assert!(!is_segment_name("journal-2026-1.jsonl"));
        assert!(!is_segment_name("journal-2026-10.jsonl.bak"));
    }

    #[test]
    fn entries_round_trip_in_order() {
        let d = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(at(2026, 10, 7, 1, 0)));
        let j = Journal::new(d.path(), clock.clone());
        j.append("registered", Some("a1"), json!({"name": "pm"}))
            .unwrap();
        clock.set(at(2026, 10, 7, 1, 5));
        j.append("closed", Some("a1"), json!({"how": "parked"}))
            .unwrap();
        j.append("note", None, json!(null)).unwrap();
        let r = j.read_all().unwrap();
        assert_eq!(r.malformed, 0);
        let kinds: Vec<_> = r.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["registered", "closed", "note"]);
        assert_eq!(r.entries[0].agent_id.as_deref(), Some("a1"));
        assert_eq!(r.entries[1].at, at(2026, 10, 7, 1, 5));
        assert_eq!(r.entries[2].agent_id, None);
    }

    #[test]
    fn a_month_boundary_starts_a_new_named_segment_and_keeps_the_old_one() {
        let d = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(at(2026, 10, 31, 23, 59)));
        let j = Journal::new(d.path(), clock.clone());
        j.append("a", None, json!(1)).unwrap();
        clock.set(at(2026, 11, 1, 0, 0));
        j.append("b", None, json!(2)).unwrap();
        let names: Vec<String> = j
            .segments()
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["journal-2026-10.jsonl", "journal-2026-11.jsonl"]);
        let kinds: Vec<_> = j
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["a", "b"], "read_all spans segments oldest first");
    }

    #[test]
    fn twenty_threads_each_with_their_own_handle_lose_nothing_and_tear_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let barrier = Arc::new(std::sync::Barrier::new(20));
        let handles: Vec<_> = (0..20)
            .map(|t| {
                let (dir, barrier) = (dir.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let j = Journal::new(dir, Arc::new(SystemClock))
                        .with_lock_timeout(std::time::Duration::from_secs(120));
                    barrier.wait();
                    for i in 0..10 {
                        j.append("tick", Some(&format!("t{t}")), json!({ "i": i }))
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let r = Journal::new(&dir, Arc::new(SystemClock))
            .read_all()
            .unwrap();
        assert_eq!(r.malformed, 0, "no torn or interleaved lines");
        assert_eq!(r.entries.len(), 200);
        // Per-thread order is preserved.
        for t in 0..20 {
            let seq: Vec<i64> = r
                .entries
                .iter()
                .filter(|e| e.agent_id.as_deref() == Some(&format!("t{t}")))
                .map(|e| e.data["i"].as_i64().unwrap())
                .collect();
            assert_eq!(seq, (0..10).collect::<Vec<_>>(), "thread {t}");
        }
    }

    #[test]
    fn a_torn_tail_is_counted_and_the_next_append_is_not_glued_onto_it() {
        let d = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(at(2026, 10, 7, 1, 0)));
        let j = Journal::new(d.path(), clock);
        j.append("first", None, json!(1)).unwrap();
        let seg = d.path().join("journal-2026-10.jsonl");
        // Simulate a crash mid-write: half an entry, no newline.
        let mut f = std::fs::OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(br#"{"at":"2026-10-07T01:00:00Z","kind":"torn","da"#)
            .unwrap();
        drop(f);
        j.append("after", None, json!(2)).unwrap();
        let r = j.read_all().unwrap();
        assert_eq!(r.malformed, 1, "the torn line is counted");
        let kinds: Vec<_> = r.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["first", "after"],
            "the entry after the tear survived"
        );
    }

    #[test]
    fn a_bad_kind_is_refused_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::new(d.path(), Arc::new(SystemClock));
        for bad in ["", "Has Space", "UPPER", "new\nline", &"x".repeat(65)] {
            assert!(
                matches!(
                    j.append(bad, None, json!(null)),
                    Err(JournalError::BadKind(_))
                ),
                "{bad:?}"
            );
        }
        assert!(j.segments().unwrap().is_empty());
        assert!(j.append("ok.kind_1-x", None, json!(null)).is_ok());
    }

    #[test]
    fn reading_a_missing_directory_is_empty_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::new(d.path().join("nope"), Arc::new(SystemClock));
        assert_eq!(j.read_all().unwrap(), ReadResult::default());
    }
}
