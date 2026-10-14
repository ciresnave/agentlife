// SPDX-License-Identifier: MIT OR Apache-2.0

//! `C:/Projects/.lane-state/<role>.json` — RESTART-TOOL-DESIGN.md §1.
//!
//! What a lane writes about itself, via hooks, inside its own process. This
//! module only reads it. ⚠️ Every field here is what the LANE claimed about
//! itself; `authorize.rs`'s identity check independently verifies the ones
//! that matter for safety (pid, cwd, session_id) against the OS rather than
//! trusting this file alone - see RESTART-TOOL-DESIGN.md §2.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneState {
    pub role: String,
    pub session_id: String,
    pub pid: u32,
    /// When `pid` started (seconds since the epoch, as `sysinfo` reports
    /// it), so the record names one process launch, not a pid the OS may
    /// reuse. `None` in files written before it was recorded, or when it
    /// could not be read.
    #[serde(default)]
    pub pid_start_secs: Option<u64>,
    pub cwd: String,
    pub name: Option<String>,
    /// `None` until `PostModelSwitch` fires at least once - hooks.md's
    /// common input fields don't include a model name at `SessionStart`,
    /// so a session that never switches models never learns it here
    /// (RESTART-TOOL-DESIGN.md §10.1). A relaunch omits `--model` entirely
    /// when this is `None`, rather than guessing.
    pub model: Option<String>,
    /// `None` until a hook payload that actually carries it arrives -
    /// PM finding, 2026-09-18: a real `SessionStart` payload does NOT
    /// include `permission_mode` (parsing on it as required failed against
    /// live input). A relaunch omits `--permission-mode` entirely when this
    /// is `None`, the same way `model` is omitted - the new session gets
    /// Claude Code's own default rather than a guessed value.
    pub permission_mode: Option<String>,
    pub remote_control: bool,
    pub busy: bool,
    pub subagents_running: u32,
    /// §1a: the lane's own claim, written when it wrote its HANDOFF, that no
    /// background shell it started is still running. `None` means "never
    /// asserted" - treated as unsafe, the same as `Some(false)`, never as
    /// `Some(true)`.
    pub no_background_shells: Option<bool>,
    /// The real, full argv `claude.exe` was launched with, read at
    /// `SessionStart` from the OS process list - PM finding, 2026-09-18
    /// (CireSnave, via the PM): CireSnave launches every lane with
    /// `--dangerously-load-development-channels server:claude-peers`,
    /// among other flags a relaunch must not silently drop. `None` when
    /// the launch command line couldn't be read this session - never
    /// preserved from a prior one, the same "always fresh, never
    /// preserved" rule `remote_control` already follows: a stale argv
    /// would be exactly as wrong as inventing one if THIS launch's real
    /// command line disagrees with it. A relaunch rebuilds its own argv
    /// from an ALLOWLIST of flags parsed out of this, never passed
    /// through blindly - see `main.rs`'s `relaunch` module.
    pub launch_args: Option<Vec<String>>,
    pub updated_at: DateTime<Utc>,
    pub updated_by_event: String,
}

impl LaneState {
    /// THE idle predicate (PM ruling 2026-10-14): not busy, no subagents running, and the lane
    /// has asserted no background shell is running. `None` for the shell claim is "never
    /// asserted", which is not idle. A fourth term the ruling names, "no unread TASK", is not
    /// recorded anywhere in this file, so it is not part of this function: a caller that needs it
    /// must read it from somewhere else. Both `status` and the restart gate (`authorize.rs`) call
    /// this, so they cannot disagree.
    pub fn is_idle(&self) -> bool {
        self.idle_blockers().is_empty()
    }

    /// Which terms of [`LaneState::is_idle`] fail, by name; empty exactly when it holds. "Never
    /// asserted" and "asserted false" are named apart, because only the first is the lane's to
    /// fix by writing its HANDOFF and running `assert-idle`.
    pub fn idle_blockers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.busy {
            out.push("busy".to_string());
        }
        if self.subagents_running > 0 {
            out.push(format!("subagents_running={}", self.subagents_running));
        }
        match self.no_background_shells {
            Some(true) => {}
            Some(false) => out.push("background_shells_asserted_present".to_string()),
            None => out.push("shells_never_asserted".to_string()),
        }
        out
    }
}

#[derive(Debug)]
pub enum LoadError {
    NotFound(String),
    Unreadable(String),
    Malformed(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(p) => write!(f, "no state file at {p}"),
            LoadError::Unreadable(e) => write!(f, "could not read state file: {e}"),
            LoadError::Malformed(e) => write!(f, "state file is not valid: {e}"),
        }
    }
}

/// The state file for `role`, under `state_dir` (normally
/// `C:/Projects/.lane-state`, RESTART-TOOL-DESIGN.md §7).
pub fn load(state_dir: &Path, role: &str) -> Result<LaneState, LoadError> {
    let path = state_dir.join(format!("{role}.json"));
    if !path.exists() {
        return Err(LoadError::NotFound(path.display().to_string()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| LoadError::Unreadable(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| LoadError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_json() -> String {
        r#"{
            "role": "overmind",
            "session_id": "3f9a1111-2222-3333-4444-555555555555",
            "pid": 48213,
            "cwd": "C:/Projects/OverMind",
            "name": "overmind",
            "model": "claude-sonnet-5",
            "permission_mode": "prompting",
            "remote_control": true,
            "busy": false,
            "subagents_running": 0,
            "no_background_shells": true,
            "updated_at": "2026-09-18T09:14:03Z",
            "updated_by_event": "Stop"
        }"#
        .to_string()
    }

    #[test]
    fn idle_needs_all_three_recorded_terms() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("overmind.json"), sample_json()).unwrap();
        let idle = load(dir.path(), "overmind").unwrap();
        assert!(idle.is_idle());
        let mut s = idle.clone();
        s.busy = true;
        assert!(!s.is_idle());
        let mut s = idle.clone();
        s.subagents_running = 1;
        assert!(!s.is_idle());
        let mut s = idle.clone();
        s.no_background_shells = Some(false);
        assert!(!s.is_idle());
        assert_eq!(s.idle_blockers(), ["background_shells_asserted_present"]);
        let mut s = idle;
        s.no_background_shells = None;
        assert!(!s.is_idle(), "never asserted is not idle");
        s.busy = true;
        s.subagents_running = 2;
        assert_eq!(
            s.idle_blockers(),
            ["busy", "subagents_running=2", "shells_never_asserted"]
        );
    }

    #[test]
    fn loads_a_well_formed_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("overmind.json"), sample_json()).unwrap();
        let state = load(dir.path(), "overmind").unwrap();
        assert_eq!(state.role, "overmind");
        assert_eq!(state.pid, 48213);
        assert_eq!(state.no_background_shells, Some(true));
    }

    #[test]
    fn missing_file_is_not_found_not_a_panic() {
        let dir = tempdir().unwrap();
        match load(dir.path(), "nonexistent") {
            Err(LoadError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_reported_not_defaulted() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("overmind.json"), "{not json").unwrap();
        match load(dir.path(), "overmind") {
            Err(LoadError::Malformed(_)) => {}
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_no_background_shells_claim_deserialises_as_none_not_true() {
        let dir = tempdir().unwrap();
        let json = sample_json().replace("\"no_background_shells\": true,", "");
        std::fs::write(dir.path().join("overmind.json"), json).unwrap();
        let state = load(dir.path(), "overmind").unwrap();
        assert_eq!(
            state.no_background_shells, None,
            "an absent claim must never silently read as an asserted true"
        );
    }
}
