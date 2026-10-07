// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry: one JSON file per **agent** (DESIGN-REVISION-1 §2.3, DESIGN-REVISION-2 §2).
//!
//! An agent is a durable identity that survives restarts; a *session* is one launch of it. The
//! registry holds **facts**, never authority: no field here grants anything (that is a person's
//! consent, DESIGN-REVISION-2 §7). One file per agent means a write never rewrites the fleet,
//! and a per-agent lock means two writers to different agents never wait on each other.
//!
//! M0 provides the types and the safe store. What fills it (the hook) is M1; what interprets
//! `intent` (park, the closed-on-purpose rule) is M2.

use crate::atomic::{is_temp_name, write_atomic};
use crate::lock::{FileLock, LockError, DEFAULT_ACQUIRE_TIMEOUT, DEFAULT_STALE_AFTER};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            Ok(Self(s))
        } else {
            Err(format!(
                "{s:?} is not a valid agent id ([A-Za-z0-9_-]{{1,64}})"
            ))
        }
    }

    /// A fresh id: `a-` and 16 hex digits drawn from the clock, the pid, a counter and the
    /// process's random hasher key. For uniqueness, not secrecy; [`Registry::create`] still
    /// refuses an id that already exists.
    pub fn generate() -> Self {
        use std::hash::{BuildHasher, Hash, Hasher};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut h);
        std::process::id().hash(&mut h);
        COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut h);
        Self(format!("a-{:016x}", h.finish()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::new(s)
    }
}

impl TryFrom<String> for AgentId {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::new(s)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> String {
        id.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosedHow {
    Parked,
    Exited,
}

/// Whether the agent should be running (DESIGN-REVISION-2 §2, -3 §2). Liveness is *not* here: it
/// is derived from the process table, never stored as truth.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Intent {
    #[default]
    Wanted,
    Lazy,
    Closed {
        how: ClosedHow,
        by: String,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// `WindowsTerminal -> shell -> claude`.
    Hand,
    /// A `lane-restart host` or `agentlife` host started it.
    Host,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub pid: u32,
    /// Seconds since the epoch; with `pid` this is the process identity.
    #[serde(default)]
    pub process_start_secs: Option<u64>,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub end_reason: Option<String>,
}

/// An explicit pin: this agent is never lazy-stopped by the idle sweep (DESIGN-REVISION-3 §5, S1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub by: String,
    pub at: DateTime<Utc>,
}

/// "This agent is waiting on the user" (DESIGN-REVISION-3 §5, S2): written by the agent itself, so
/// that it is not shut down while a person is directly interacting with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingMark {
    /// What it waits on; `"user"` today.
    pub on: String,
    pub since: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub schema: u32,
    pub agent_id: AgentId,
    #[serde(default)]
    pub name: Option<String>,
    /// The directory the agent was **launched** in, not where it has wandered since.
    pub launch_cwd: String,
    /// Self-claimed and advisory (`LANE_ROLE`); never an authority.
    #[serde(default)]
    pub role: Option<String>,
    pub first_seen: DateTime<Utc>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// The argv exactly as launched; a restore rebuilds from it through an allowlist.
    #[serde(default)]
    pub launch_args: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub remote_control: bool,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub intent: Intent,
    #[serde(default)]
    pub pinned: Option<Pin>,
    #[serde(default)]
    pub waiting: Option<WaitingMark>,
}

impl AgentRecord {
    /// A new record with no sessions, `Wanted`, schema [`SCHEMA`].
    pub fn new(
        agent_id: AgentId,
        launch_cwd: impl Into<String>,
        first_seen: DateTime<Utc>,
    ) -> Self {
        Self {
            schema: SCHEMA,
            agent_id,
            name: None,
            launch_cwd: launch_cwd.into(),
            role: None,
            first_seen,
            sessions: Vec::new(),
            launch_args: None,
            model: None,
            permission_mode: None,
            remote_control: false,
            origin: Origin::default(),
            intent: Intent::default(),
            pinned: None,
            waiting: None,
        }
    }
}

#[derive(Debug)]
pub enum RegistryError {
    AlreadyExists(AgentId),
    NotFound(AgentId),
    /// A mutation changed the record's `agent_id`; refused, because the id is the file name.
    IdChanged {
        expected: AgentId,
        got: AgentId,
    },
    /// The file declares a schema this build does not understand.
    UnsupportedSchema {
        id: String,
        schema: u32,
    },
    Lock(LockError),
    Io(std::io::Error),
    Json {
        path: PathBuf,
        why: String,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::AlreadyExists(id) => write!(f, "agent {id} already exists"),
            RegistryError::NotFound(id) => write!(f, "no agent {id}"),
            RegistryError::IdChanged { expected, got } => {
                write!(f, "refusing to change agent id {expected} to {got}")
            }
            RegistryError::UnsupportedSchema { id, schema } => {
                write!(
                    f,
                    "agent {id} has schema {schema}; this build understands {SCHEMA}"
                )
            }
            RegistryError::Lock(e) => write!(f, "{e}"),
            RegistryError::Io(e) => write!(f, "registry I/O error: {e}"),
            RegistryError::Json { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        RegistryError::Io(e)
    }
}

/// What a listing found: every readable record, and a note for each file it could not use.
/// One corrupt file never hides the rest of the fleet.
#[derive(Debug, Default)]
pub struct Listing {
    pub records: Vec<AgentRecord>,
    pub problems: Vec<(PathBuf, String)>,
}

pub struct Registry {
    dir: PathBuf,
    lock_timeout: Duration,
}

impl Registry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            lock_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }

    /// How long a write waits for an agent's lock (see `Journal::with_lock_timeout`).
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn record_path(&self, id: &AgentId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn lock(&self, id: &AgentId) -> Result<FileLock, RegistryError> {
        FileLock::acquire(
            self.dir.join(format!("{id}.lock")),
            self.lock_timeout,
            DEFAULT_STALE_AFTER,
        )
        .map_err(RegistryError::Lock)
    }

    fn read_at(path: &Path, id: &str) -> Result<AgentRecord, RegistryError> {
        let text = std::fs::read_to_string(path)?;
        let rec: AgentRecord = serde_json::from_str(&text).map_err(|e| RegistryError::Json {
            path: path.to_path_buf(),
            why: e.to_string(),
        })?;
        if rec.schema != SCHEMA {
            return Err(RegistryError::UnsupportedSchema {
                id: id.to_string(),
                schema: rec.schema,
            });
        }
        if rec.agent_id.as_str() != id {
            return Err(RegistryError::Json {
                path: path.to_path_buf(),
                why: format!("file name says {id} but the record says {}", rec.agent_id),
            });
        }
        Ok(rec)
    }

    fn write(&self, rec: &AgentRecord) -> Result<(), RegistryError> {
        let mut bytes = serde_json::to_vec_pretty(rec).map_err(|e| RegistryError::Json {
            path: self.record_path(&rec.agent_id),
            why: e.to_string(),
        })?;
        bytes.push(b'\n');
        write_atomic(&self.record_path(&rec.agent_id), &bytes)?;
        Ok(())
    }

    pub fn get(&self, id: &AgentId) -> Result<Option<AgentRecord>, RegistryError> {
        let path = self.record_path(id);
        match std::fs::metadata(&path) {
            Ok(_) => Self::read_at(&path, id.as_str()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Creates a record; refuses if the id is taken.
    pub fn create(&self, rec: &AgentRecord) -> Result<(), RegistryError> {
        std::fs::create_dir_all(&self.dir)?;
        let _g = self.lock(&rec.agent_id)?;
        if self.record_path(&rec.agent_id).exists() {
            return Err(RegistryError::AlreadyExists(rec.agent_id.clone()));
        }
        self.write(rec)
    }

    /// Read-modify-write under the agent's lock. `f` may not change the id.
    pub fn update<F: FnOnce(&mut AgentRecord)>(
        &self,
        id: &AgentId,
        f: F,
    ) -> Result<AgentRecord, RegistryError> {
        std::fs::create_dir_all(&self.dir)?;
        let _g = self.lock(id)?;
        let mut rec = self
            .get(id)?
            .ok_or_else(|| RegistryError::NotFound(id.clone()))?;
        f(&mut rec);
        if &rec.agent_id != id {
            return Err(RegistryError::IdChanged {
                expected: id.clone(),
                got: rec.agent_id,
            });
        }
        self.write(&rec)?;
        Ok(rec)
    }

    /// Like [`update`](Self::update), but creates the record from `create` when it is missing,
    /// all under one lock. Returns the record and whether it was created.
    pub fn upsert<C, F>(
        &self,
        id: &AgentId,
        create: C,
        f: F,
    ) -> Result<(AgentRecord, bool), RegistryError>
    where
        C: FnOnce() -> AgentRecord,
        F: FnOnce(&mut AgentRecord),
    {
        std::fs::create_dir_all(&self.dir)?;
        let _g = self.lock(id)?;
        let (mut rec, created) = match self.get(id)? {
            Some(r) => (r, false),
            None => (create(), true),
        };
        f(&mut rec);
        if &rec.agent_id != id {
            return Err(RegistryError::IdChanged {
                expected: id.clone(),
                got: rec.agent_id,
            });
        }
        self.write(&rec)?;
        Ok((rec, created))
    }

    /// Every agent, sorted by id. Temp files and lock files are skipped; a file that cannot be
    /// used is reported in `problems` and does not stop the listing.
    pub fn list(&self) -> Result<Listing, RegistryError> {
        let mut out = Listing::default();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for e in rd {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if is_temp_name(&name) || !name.ends_with(".json") {
                continue;
            }
            paths.push(e.path());
        }
        paths.sort();
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match Self::read_at(&path, &stem) {
                Ok(r) => out.records.push(r),
                Err(e) => out.problems.push((path, e.to_string())),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::{Arc, Barrier};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 1, 0, 0).unwrap()
    }

    fn rec(id: &str) -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new(id).unwrap(), "C:/Projects/x", t0());
        r.name = Some("x".into());
        r
    }

    fn session(n: u32) -> Session {
        Session {
            session_id: format!("s{n}"),
            pid: n,
            process_start_secs: Some(1000 + n as u64),
            started_at: t0(),
            ended_at: None,
            end_reason: None,
        }
    }

    #[test]
    fn agent_ids_are_validated_and_generated_ones_are_valid_and_distinct() {
        for ok in ["a", "pm", "a-0123456789abcdef", "A_b-9"] {
            assert!(AgentId::new(ok).is_ok(), "{ok}");
        }
        for bad in ["", "a b", "a/b", "a\\b", "a;b", "..", &"x".repeat(65)] {
            assert!(AgentId::new(bad).is_err(), "{bad:?}");
        }
        let ids: std::collections::HashSet<_> = (0..2000).map(|_| AgentId::generate()).collect();
        assert_eq!(ids.len(), 2000, "generated ids must not collide");
        assert!(ids.iter().all(|i| AgentId::new(i.as_str()).is_ok()));
    }

    #[test]
    fn a_record_round_trips_through_the_store() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        let mut r = rec("a1");
        r.sessions.push(session(1));
        r.launch_args = Some(vec!["claude".into(), "--model".into(), "sonnet".into()]);
        r.intent = Intent::Closed {
            how: ClosedHow::Parked,
            by: "person".into(),
            at: t0(),
        };
        reg.create(&r).unwrap();
        assert_eq!(reg.get(&r.agent_id).unwrap(), Some(r));
        assert_eq!(reg.get(&AgentId::new("nope").unwrap()).unwrap(), None);
    }

    #[test]
    fn create_refuses_an_id_that_exists() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        reg.create(&rec("a1")).unwrap();
        assert!(matches!(
            reg.create(&rec("a1")),
            Err(RegistryError::AlreadyExists(_))
        ));
    }

    #[test]
    fn update_requires_the_record_and_cannot_change_its_id() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        let id = AgentId::new("a1").unwrap();
        assert!(matches!(
            reg.update(&id, |_| {}),
            Err(RegistryError::NotFound(_))
        ));
        reg.create(&rec("a1")).unwrap();
        let r = reg.update(&id, |r| r.model = Some("opus".into())).unwrap();
        assert_eq!(r.model.as_deref(), Some("opus"));
        let err = reg
            .update(&id, |r| r.agent_id = AgentId::new("evil").unwrap())
            .unwrap_err();
        assert!(matches!(err, RegistryError::IdChanged { .. }), "{err}");
        // The refused change wrote nothing: the old record is intact and no 'evil' file exists.
        assert_eq!(
            reg.get(&id).unwrap().unwrap().model.as_deref(),
            Some("opus")
        );
        assert!(reg.get(&AgentId::new("evil").unwrap()).unwrap().is_none());
    }

    #[test]
    fn upsert_creates_once_and_then_updates() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        let id = AgentId::new("a1").unwrap();
        let (r, created) = reg
            .upsert(&id, || rec("a1"), |r| r.sessions.push(session(1)))
            .unwrap();
        assert!(created);
        assert_eq!(r.sessions.len(), 1);
        let (r, created) = reg
            .upsert(
                &id,
                || panic!("must not create twice"),
                |r| r.sessions.push(session(2)),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(r.sessions.len(), 2);
    }

    #[test]
    fn twenty_threads_updating_one_agent_lose_no_session() {
        let d = tempfile::tempdir().unwrap();
        let reg = Arc::new(Registry::new(d.path()).with_lock_timeout(Duration::from_secs(120)));
        let id = AgentId::new("shared").unwrap();
        reg.create(&rec("shared")).unwrap();
        let barrier = Arc::new(Barrier::new(20));
        let handles: Vec<_> = (0..20u32)
            .map(|t| {
                let (reg, id, barrier) = (reg.clone(), id.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    for i in 0..10u32 {
                        reg.update(&id, |r| r.sessions.push(session(t * 100 + i)))
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let r = reg.get(&id).unwrap().unwrap();
        let ids: std::collections::HashSet<_> =
            r.sessions.iter().map(|s| s.session_id.clone()).collect();
        assert_eq!(r.sessions.len(), 200, "lost updates");
        assert_eq!(ids.len(), 200, "duplicated or overwritten sessions");
    }

    #[test]
    fn listing_skips_temp_and_lock_files_and_survives_a_corrupt_one() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        reg.create(&rec("b2")).unwrap();
        reg.create(&rec("a1")).unwrap();
        std::fs::write(d.path().join(".a1.json.tmp.9.0"), "{half").unwrap();
        std::fs::write(d.path().join("a1.lock"), "tok").unwrap();
        std::fs::write(d.path().join("corrupt.json"), "{nope").unwrap();
        let l = reg.list().unwrap();
        let ids: Vec<_> = l
            .records
            .iter()
            .map(|r| r.agent_id.as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            ["a1", "b2"],
            "sorted, and the corrupt file did not hide the others"
        );
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert!(l.problems[0].0.ends_with("corrupt.json"));
    }

    #[test]
    fn a_record_whose_name_and_content_disagree_is_a_problem_not_a_record() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        reg.create(&rec("a1")).unwrap();
        std::fs::copy(d.path().join("a1.json"), d.path().join("zz.json")).unwrap();
        let l = reg.list().unwrap();
        assert_eq!(l.records.len(), 1);
        assert_eq!(l.problems.len(), 1);
        assert!(l.problems[0].1.contains("zz"), "{}", l.problems[0].1);
    }

    #[test]
    fn a_future_schema_is_refused_not_misread_and_unknown_fields_are_tolerated() {
        let d = tempfile::tempdir().unwrap();
        let reg = Registry::new(d.path());
        reg.create(&rec("a1")).unwrap();
        let p = d.path().join("a1.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["added_by_a_later_version"] = serde_json::json!({"x": 1});
        std::fs::write(&p, v.to_string()).unwrap();
        assert!(
            reg.get(&AgentId::new("a1").unwrap()).is_ok(),
            "additive fields are fine"
        );
        v["schema"] = serde_json::json!(2);
        std::fs::write(&p, v.to_string()).unwrap();
        assert!(matches!(
            reg.get(&AgentId::new("a1").unwrap()),
            Err(RegistryError::UnsupportedSchema { schema: 2, .. })
        ));
    }

    #[test]
    fn the_intent_wire_shape_is_stable() {
        // Other tools (and later milestones) read this JSON, so its shape is pinned.
        let wanted = serde_json::to_value(Intent::Wanted).unwrap();
        assert_eq!(wanted, serde_json::json!({"state": "wanted"}));
        let closed = serde_json::to_value(Intent::Closed {
            how: ClosedHow::Exited,
            by: "person".into(),
            at: t0(),
        })
        .unwrap();
        assert_eq!(
            closed,
            serde_json::json!({"state": "closed", "how": "exited", "by": "person", "at": "2026-10-07T01:00:00Z"})
        );
        assert_eq!(
            serde_json::to_value(Intent::Lazy).unwrap(),
            serde_json::json!({"state": "lazy"})
        );
    }

    #[test]
    fn a_minimal_record_from_another_writer_still_parses_with_defaults() {
        let json = r#"{"schema":1,"agent_id":"a1","launch_cwd":"C:/x","first_seen":"2026-10-07T01:00:00Z"}"#;
        let r: AgentRecord = serde_json::from_str(json).unwrap();
        assert_eq!(r.intent, Intent::Wanted);
        assert!(r.sessions.is_empty());
        assert_eq!(r.origin, Origin::Other);
        assert!(!r.remote_control);
    }
}
