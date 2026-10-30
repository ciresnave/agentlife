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
pub use lane_state::{facts, paths, permission_mode, role, state};

/// `lane_state::platform::diag::start_readings`, re-exported for the binary.
pub fn platform_diag(pid: u32) -> String {
    lane_state::platform::diag::start_readings(pid)
}

pub mod approvals;
pub mod authorize;
pub mod disk;
pub mod duplicate;
pub mod folder_trust;
pub mod handlers;
pub mod handoff_lint;
pub mod host;
pub mod host_boot;
pub mod install;
pub mod lane_new;
pub mod lane_new_host;
pub mod lane_new_remote;
pub mod lane_state_writer;
pub mod lane_stop;
pub mod launch;
pub mod launcher_gate;
pub mod ledger;
pub mod log;
pub mod marker;
pub mod notify;
pub mod peers_join;
pub mod relaunch;
pub mod spec;
pub mod spec_import;
pub mod spec_retire;
pub mod spec_synapse;
pub mod start;
pub mod start_lock;
pub mod stop;
pub mod tab_close;
#[cfg(test)]
mod testing;
pub mod watch;
pub mod wsl;
pub mod wsl_facts;

/// The state directory (`C:/Projects/.lane-state` on Windows; under `XDG_STATE_HOME` elsewhere, see
/// `lane_state::platform`) - RESTART-TOOL-DESIGN.md section 7. Not
/// configurable via CLI on purpose: a caller-supplied state directory would
/// defeat the whole point of a fixed, portfolio-wide location every lane and
/// the PM agree on. (Moved here from the binary with `relaunch`, which reads
/// the policy default model from it; the path is unchanged.)
pub fn state_dir() -> std::path::PathBuf {
    lane_state::platform::state_dir()
}
