// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart ledger` - read-only: prints `restart.log` filtered. Writes nothing.
//!
//! Moved out of `app.rs` unchanged apart from reading the log from the context's state directory
//! and writing its text through the [`Ctx`] (thin-main PR 5).

use super::ctx::Ctx;
use crate::ledger;
use std::process::ExitCode;

pub(super) fn run(
    ctx: &mut Ctx,
    mut f: ledger::Filter,
    json: bool,
    since: Option<String>,
    by_lane: bool,
) -> ExitCode {
    if let Some(s) = since {
        match ledger::parse_since(&s, chrono::Utc::now()) {
            Ok(t) => f.since = Some(t),
            Err(e) => {
                ctx.errln(&format!("lane-restart ledger: {e}"));
                return ExitCode::FAILURE;
            }
        }
    }
    let read = match ledger::read(&ctx.state_dir.join("restart.log")) {
        Ok(r) => r,
        Err(e) => {
            ctx.errln(&format!("lane-restart ledger: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let shown = ledger::select(&read.rows, &f);
    if by_lane {
        ctx.out(&ledger::render_by_lane(&shown, read.rows.len()));
        return ExitCode::SUCCESS;
    }
    if json {
        for r in &shown {
            match serde_json::to_string(r) {
                Ok(l) => ctx.outln(&l),
                Err(e) => {
                    ctx.errln(&format!("lane-restart ledger: {e}"));
                    return ExitCode::FAILURE;
                }
            }
        }
        ctx.errln(&format!(
            "({} of {} entries shown; {} unreadable line(s))",
            shown.len(),
            read.rows.len(),
            read.unreadable.len()
        ));
    } else {
        ctx.out(&ledger::render(&shown, read.rows.len(), &read.unreadable));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
