// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tiny pointer from a **process** (pid + start time) to the agent that owns it.
//!
//! `SessionEnd` runs under a documented 1.5 s budget, and finding "which agent is this process"
//! by reading every record would scale with the fleet. One small file per process makes that an
//! O(1) read. The key is pid **and** start time, so a recycled pid never resolves to the wrong
//! agent (DESIGN.md §2.4).

use crate::atomic::write_atomic;
use crate::registry::AgentId;
use std::path::PathBuf;

pub struct ProcIndex {
    dir: PathBuf,
}

impl ProcIndex {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, pid: u32, start_secs: u64) -> PathBuf {
        self.dir.join(format!("{pid}-{start_secs}.id"))
    }

    pub fn put(&self, pid: u32, start_secs: u64, id: &AgentId) -> std::io::Result<()> {
        write_atomic(&self.path(pid, start_secs), id.as_str().as_bytes())
    }

    /// The agent recorded for exactly this process, if any. An unreadable or invalid pointer is
    /// treated as absent, never as some other agent.
    pub fn get(&self, pid: u32, start_secs: u64) -> Option<AgentId> {
        std::fs::read_to_string(self.path(pid, start_secs))
            .ok()
            .and_then(|s| AgentId::new(s.trim()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_round_trips_and_is_keyed_by_pid_and_start_time() {
        let d = tempfile::tempdir().unwrap();
        let idx = ProcIndex::new(d.path());
        let a = AgentId::new("a1").unwrap();
        idx.put(10, 1000, &a).unwrap();
        assert_eq!(idx.get(10, 1000), Some(a));
        // The same pid with a different start time is a different process: no answer.
        assert_eq!(idx.get(10, 2000), None);
        assert_eq!(idx.get(11, 1000), None);
    }

    #[test]
    fn a_later_put_for_the_same_process_replaces_the_earlier_one() {
        let d = tempfile::tempdir().unwrap();
        let idx = ProcIndex::new(d.path());
        idx.put(10, 1000, &AgentId::new("a1").unwrap()).unwrap();
        idx.put(10, 1000, &AgentId::new("a2").unwrap()).unwrap();
        assert_eq!(idx.get(10, 1000), Some(AgentId::new("a2").unwrap()));
    }

    #[test]
    fn a_corrupt_pointer_is_absent_not_another_agent() {
        let d = tempfile::tempdir().unwrap();
        let idx = ProcIndex::new(d.path());
        std::fs::write(d.path().join("10-1000.id"), "not a valid id!!").unwrap();
        assert_eq!(idx.get(10, 1000), None);
        std::fs::write(d.path().join("11-1000.id"), "").unwrap();
        assert_eq!(idx.get(11, 1000), None);
    }
}
