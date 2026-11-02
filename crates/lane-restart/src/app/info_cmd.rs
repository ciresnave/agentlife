// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart --help` and `--version`: text only, no state read or written.
//!
//! `--version` is RESTART-TOOL-DESIGN.md §12.9: the crate version, that it embeds no approvals, and
//! where it would fetch them from. No network: `lane-restart approvals` does the fetch.
//!
//! Moved out of `app.rs` unchanged apart from reading the home directory and writing its text
//! through the [`Ctx`] (thin-main PR 16, the last of the series).

use super::ctx::Ctx;
use std::process::ExitCode;

pub(super) fn help(ctx: &mut Ctx) -> ExitCode {
    ctx.out(super::USAGE);
    ExitCode::SUCCESS
}

pub(super) fn version(ctx: &mut Ctx) -> ExitCode {
    use crate::approvals;
    ctx.outln(&format!("lane-restart {}", env!("CARGO_PKG_VERSION")));
    ctx.outln("embedded approvals: none (by design - RESTART-TOOL-DESIGN.md §12.2)");
    match ctx.home() {
        Some(home) => match approvals::load_config(&approvals::config_path(&home)) {
            Ok(c) => ctx.outln(&format!(
                "approvals source: {}:{} (default branch), notify role: {}",
                c.approvals.repo, c.approvals.path, c.notify_role
            )),
            Err(e) => ctx.outln(&format!("approvals source: {e}")),
        },
        None => ctx.outln("approvals source: no home directory"),
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
