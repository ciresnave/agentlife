// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife`: agent lifecycle control. See `docs/DESIGN*.md` and `docs/MILESTONES.md`.
//!
//! Milestone M0 (foundations): nothing here acts on a real agent. The modules are the
//! plumbing every later milestone stands on: where state lives (`home`), how it is
//! configured (`config`), how a write is made safe (`atomic`, `lock`), the append-only
//! `journal`, the per-agent `registry`, and how a process is identified (`identity`).

pub mod atomic;
pub mod clock;
pub mod config;
pub mod home;
pub mod identity;
pub mod install;
pub mod journal;
pub mod lock;
pub mod registry;

/// The crate version, as one string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
