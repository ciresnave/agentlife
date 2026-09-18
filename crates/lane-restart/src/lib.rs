// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restarts a Claude Code lane session with the same identity.
//!
//! Design: `RESTART-TOOL-DESIGN.md` at the repo root. This is the "dangerous
//! part" the design spec exists to constrain — every module here is written
//! so the DECISION to kill and relaunch is a pure function over injected
//! facts (`SystemFacts`), testable without touching a real process, and the
//! only place real OS calls happen is behind that one trait's real
//! implementation, wired in `main.rs`.

pub mod authorize;
pub mod facts;
pub mod lane_state_writer;
pub mod log;
pub mod state;
