// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/DESIGN*.md` and `docs/MILESTONES.md`.
//!
//! M0 (foundations): where state lives (`home`), how it is configured (`config`), how a write
//! is made safe (`atomic`, `lock`), the append-only `journal`, the per-agent `registry`, and
//! how a process is identified (`identity`). M1 (the hook): `hook` registers an agent from
//! `SessionStart`/`SessionEnd` (`procindex` finds it again in O(1)), and `list` shows the
//! registry joined with the process table. M2a (intent): `intent` decides whether an agent was
//! closed on purpose, `marks` records a pin or a waiting-on-the-user mark, `control` parks, stops
//! or unparks an agent that is **not running**, `caller` tells a person from an agent and
//! `select` names an agent. Nothing here touches a running process.

pub mod atomic;
pub mod caller;
pub mod cli;
pub mod clock;
pub mod config;
pub mod consent;
pub mod control;
pub mod down;
pub mod home;
pub mod hook;
pub mod identity;
pub mod install;
pub mod intent;
pub mod journal;
pub mod launch;
pub mod list;
pub mod lock;
pub mod marks;
pub mod peers;
pub mod pending;
pub mod plan;
pub mod procindex;
pub mod readiness;
pub mod registry;
pub mod restore;
pub mod select;
pub mod summary;
pub mod task;

/// The crate version, as one string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
