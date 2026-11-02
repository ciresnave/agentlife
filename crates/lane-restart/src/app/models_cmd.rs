// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart models` - each lane's recorded model, one per line, with Opus flagged. Read-only.
//!
//! Moved out of `app.rs` unchanged apart from reading the context's state directory and writing
//! its text through the [`Ctx`] (thin-main PR 16).

use super::ctx::Ctx;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx) -> ExitCode {
    let report = crate::relaunch::models_report(&ctx.state_dir);
    ctx.out(&report);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
