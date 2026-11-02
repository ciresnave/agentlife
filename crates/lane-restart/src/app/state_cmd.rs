// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart state <event>` - the hook command. Handled separately from `cli::restart` because
//! its shape (a positional subcommand, then a single positional event name) doesn't fit the restart
//! CLI's own flags at all, and mixing the two would make either grammar harder to read. It stays a
//! slice pattern in `run_with` (`[cmd, event]`): there is no flag loop to replace, and it is the hook
//! hot path.
//!
//! HOT PATH: runs on every tool call of every lane. Nothing here may add work: the real [`Ctx`]
//! builds nothing eagerly, and the process-parent walk (one whole-process scan) is built when the
//! hook asks for it, exactly as the hook built it before.
//!
//! Moved out of `app.rs` unchanged apart from reading its inputs (stdin, the parent walk,
//! `LANE_ROLE`) and writing its text through the [`Ctx`] (thin-main PR 15).

use super::ctx::Ctx;
use crate::lane_state_writer;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, event: &str) -> ExitCode {
    let started = std::time::Instant::now();
    let my_pid = std::process::id();
    let lane_role_env = ctx.lane_role.clone();
    let mut stdin = ctx.stdin();
    let lookup = ctx.parents();
    let result = lane_state_writer::run(
        &ctx.state_dir,
        event,
        my_pid,
        &*lookup,
        lane_role_env.as_deref(),
        &mut *stdin,
    );
    if let Some(note) =
        lane_state_writer::slow_note(event, started.elapsed(), ctx.parent_full_scans())
    {
        ctx.errln(&note);
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ctx.errln(&format!("lane-restart state {event}: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
