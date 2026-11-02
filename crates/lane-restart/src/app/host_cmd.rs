// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart host --role <role> -- <child argv...>` - RESTART-TOOL-DESIGN.md §12.5. Runs INSIDE
//! the relaunched session's own terminal tab (launched by `spawn_relaunch`) and hosts `claude` in a
//! ConPTY this process owns.
//!
//! Moved out of `app.rs` unchanged apart from writing the boot record into the context's state
//! directory, running the host through the context, and writing its text through it (thin-main
//! PR 14). The record is still written BEFORE the arguments are parsed, so it is there even when
//! they are wrong.

use super::ctx::Ctx;
use crate::cli;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, rest: &[String]) -> ExitCode {
    // Before anything can fail or wait: a restart that finds no record knows the host never ran.
    crate::host_boot::record(&ctx.state_dir, rest);
    match cli::host(rest) {
        Ok((role, child_argv)) => {
            let host = ctx.host_runner();
            match (*host)(&role, &child_argv) {
                Ok(outcome) => match outcome.child_exit_code {
                    Some(0) | None => ExitCode::SUCCESS,
                    Some(_) => ExitCode::FAILURE,
                },
                Err(e) => {
                    ctx.errln(&format!("lane-restart host: {e}"));
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            ctx.errln(&format!("lane-restart host: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
