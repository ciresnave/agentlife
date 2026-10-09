// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restarts a Claude Code lane session with the same identity.
//!
//! Design: `RESTART-TOOL-DESIGN.md` at the repo root. This is the "dangerous
//! part" the design spec exists to constrain — every module here is written
//! so the DECISION to kill and relaunch is a pure function over injected
//! facts (`SystemFacts`), testable without touching a real process, and the
//! only place real OS calls happen is behind that one trait's real
//! implementation, wired in `main.rs`.

// Moved to the `lane-state` crate; the old paths keep working.
pub use lane_state::{facts, paths, state};

pub mod approvals;
pub mod authorize;
pub mod handlers;
pub mod host;
pub mod lane_state_writer;
pub mod launch;
pub mod log;
pub mod notify;
pub mod relaunch;
pub mod stop;
pub mod tab_close;
#[cfg(test)]
mod testing;

/// `C:/Projects/.lane-state` - RESTART-TOOL-DESIGN.md section 7. Not
/// configurable via CLI on purpose: a caller-supplied state directory would
/// defeat the whole point of a fixed, portfolio-wide location every lane and
/// the PM agree on. (Moved here from the binary with `relaunch`, which reads
/// the policy default model from it; the path is unchanged.)
pub fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("C:/Projects/.lane-state")
}
