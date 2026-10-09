// SPDX-License-Identifier: MIT OR Apache-2.0

//! Test doubles shared by the stop / start / restart tests: a `SystemFacts`
//! that records every call in order, and a `StateReader` that plays back a
//! script. Compiled only for tests.

use crate::facts::{KillError, ProcEntry, ProcessIdentity, ShellCheckError, SystemFacts};
use crate::relaunch::StateReader;
use crate::state::LaneState;
use chrono::{DateTime, TimeZone, Utc};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// One shared, ordered log of what happened.
pub type Log = Rc<RefCell<Vec<String>>>;

pub fn log() -> Log {
    Rc::new(RefCell::new(Vec::new()))
}

/// `now()` is fixed; `kill_verified` can be made to fail; the liveness poll
/// (`find_claude_process_in`) records the `after` it was given and answers
/// `alive`. Every call is logged. Nothing else is allowed.
pub struct RecordingFacts {
    pub log: Log,
    pub now: DateTime<Utc>,
    /// `kill_verified` refuses with `IdentityChanged` when true.
    pub kill_refuses: bool,
    pub alive: bool,
}

impl RecordingFacts {
    pub fn new(log: &Log) -> Self {
        RecordingFacts {
            log: Rc::clone(log),
            now: Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap(),
            kill_refuses: false,
            alive: true,
        }
    }
}

impl SystemFacts for RecordingFacts {
    fn is_alive_claude_process(&self, _: u32) -> bool {
        unreachable!("not asked")
    }
    fn cwd_of(&self, _: u32) -> Option<std::path::PathBuf> {
        unreachable!("not asked")
    }
    fn has_live_shell_descendant(&self, _: u32) -> Result<bool, ShellCheckError> {
        unreachable!("not asked")
    }
    fn transcript_is_recent(&self, _: &str, _: &str, _: Duration) -> bool {
        unreachable!("not asked")
    }
    fn now(&self) -> DateTime<Utc> {
        self.log.borrow_mut().push("now".into());
        self.now
    }
    fn process_identity(&self, _: u32) -> Option<ProcessIdentity> {
        unreachable!("not asked")
    }
    fn kill_verified(&self, pid: u32, expected: &ProcessIdentity) -> Result<(), KillError> {
        self.log
            .borrow_mut()
            .push(format!("kill {pid} start={}", expected.start_time_secs));
        if self.kill_refuses {
            Err(KillError::IdentityChanged)
        } else {
            Ok(())
        }
    }
    fn find_claude_process_in(&self, cwd: &str, after: u64) -> Option<u32> {
        self.log
            .borrow_mut()
            .push(format!("poll {cwd} after={after}"));
        self.alive.then_some(7)
    }
    fn process_table(&self) -> Result<Vec<ProcEntry>, ShellCheckError> {
        unreachable!("not asked")
    }
}

/// Plays `script` back one entry per read; the last entry repeats. Logs reads.
pub struct ScriptedReader {
    pub log: Log,
    pub script: Vec<Option<LaneState>>,
    pub reads: RefCell<usize>,
}

impl ScriptedReader {
    pub fn new(log: &Log, script: Vec<Option<LaneState>>) -> Self {
        ScriptedReader {
            log: Rc::clone(log),
            script,
            reads: RefCell::new(0),
        }
    }
}

impl StateReader for ScriptedReader {
    fn read(&self, role: &str) -> Option<LaneState> {
        self.log.borrow_mut().push(format!("read {role}"));
        let mut n = self.reads.borrow_mut();
        let idx = (*n).min(self.script.len() - 1);
        *n += 1;
        self.script[idx].clone()
    }
}

/// A state file at `session` whose last event was `event`.
pub fn state_at(role: &str, session: &str, event: &str) -> LaneState {
    LaneState {
        role: role.to_string(),
        session_id: session.to_string(),
        pid: 4242,
        pid_start_secs: None,
        cwd: "C:/Projects/x".to_string(),
        name: None,
        model: Some("claude-sonnet-5".to_string()),
        permission_mode: Some("prompting".to_string()),
        remote_control: false,
        busy: false,
        subagents_running: 0,
        no_background_shells: Some(true),
        launch_args: None,
        updated_at: Utc::now(),
        updated_by_event: event.to_string(),
    }
}
