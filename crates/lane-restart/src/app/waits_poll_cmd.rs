// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart waits-poll [--dry-run]` - one supervisor pass over every live role's `WAIT:` lines
//! (`waits_poll.rs`). Tells a lane once when a wait is due. Read-only apart from the ledger and the
//! notice itself; it registers nothing and leaves nothing running.
//!
//! Moved out of `app.rs` unchanged apart from reading its inputs (the process facts, the check
//! runner) and sending its notice through the [`Ctx`] (thin-main PR 3).

use super::ctx::Ctx;
use crate::spec::{self, Lifecycle, SpecStatus};
use crate::waits_poll::{self, Ledger, Outcome, Subject};
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, dry_run: bool) -> ExitCode {
    let state = ctx.state_dir.clone();
    let report = spec::status_all(&state, ctx.facts());
    // Live (the recorded process is running: only then is `activity` present), a complete spec, a
    // HANDOFF to read; never the PM's own.
    let subjects: Vec<Subject> = report
        .roles
        .iter()
        .filter(|r| {
            r.role != "pm"
                && r.state == Lifecycle::Running
                && r.activity.is_some()
                && r.spec == SpecStatus::Complete
        })
        .filter_map(|r| {
            let path = std::path::PathBuf::from(&r.handoff.as_ref()?.path);
            let handoff = std::fs::read_to_string(&path).ok()?;
            Some(Subject {
                role: r.role.clone(),
                cwd: path.parent()?.to_path_buf(),
                handoff,
            })
        })
        .collect();
    let now = chrono::Utc::now();
    let due = waits_poll::poll(&subjects, now, ctx.runner());
    let mut ledger = Ledger::load(&state);
    let results = waits_poll::deliver(&due, &mut ledger, dry_run, &mut |role, text| {
        ctx.send(role, text)
    });
    ctx.outln(&format!(
        "waits-poll: {} live role(s) read, {} wait(s) due{}",
        subjects.len(),
        due.len(),
        if dry_run {
            " (dry run: nothing sent)"
        } else {
            ""
        }
    ));
    let mut any_sent = false;
    for (d, o) in &results {
        any_sent |= *o == Outcome::Sent;
        ctx.outln(&format!(
            "{}\t{}\t{}\t{:?}",
            d.role,
            d.reason.slug(),
            d.wait.event,
            o
        ));
    }
    if any_sent {
        if let Err(e) = ledger.save(&state) {
            ctx.errln(&format!("lane-restart waits-poll: could not save the ledger ({e}); the next pass may repeat a notice"));
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
