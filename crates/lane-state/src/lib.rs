// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a Claude Code lane wrote about itself, and the pure rules for reading
//! a lane's process and launch line. Shared by `lane-restart`, `with-secret`
//! and agentlife, so none of them copies another's code (the portfolio rule:
//! cross-repo dependencies are published versions).
//!
//! - `state`: the `<role>.json` state file a lane's hooks write.
//! - `facts`: the OS facts about processes (`SystemFacts`, `SysinfoFacts`).
//! - `paths`: path comparison and the transcript directory name.
//! - `platform`: where the portfolio's fixed directories live on this OS.
//! - `role`: the one definition of a lane role (`[A-Za-z0-9_-]{1,64}`).
//! - `claude_proc`: finding the `claude` pid, parsing its launch flags, the
//!   session-identity environment variables.

pub mod claude_proc;
pub mod facts;
pub mod paths;
pub mod platform;
pub mod role;
pub mod state;
