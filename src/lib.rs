// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/DESIGN*.md` and `docs/MILESTONES.md`.
//!
//! M0 (foundations): where state lives (`home`), how it is configured (`config`), how a write
//! is made safe (`atomic`, `lock`), the append-only `journal`, the per-agent `registry`, and
//! how a process is identified (`identity`). M1 (the hook): `hook` registers an agent from
//! `SessionStart`/`SessionEnd` (`procindex` finds it again in O(1)), and `list` shows the
//! registry joined with the process table. Neither acts on a running agent.

pub mod atomic;
pub mod claude_proc;
pub mod cli;
pub mod clock;
pub mod config;
pub mod home;
pub mod hook;
pub mod identity;
pub mod install;
pub mod journal;
pub mod list;
pub mod lock;
pub mod procindex;
pub mod registry;

/// The crate version, as one string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
