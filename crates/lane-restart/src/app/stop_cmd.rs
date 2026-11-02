// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart stop --role R --reason <text> [--self] [--soft|--hard] [--dry-run] [--yes]` -
//! 14.3. Suspends a lane: the complete spec first, then the verified kill, then the spec marked
//! `suspended`. Exit codes: 0 stopped and suspended, 1 refused or failed, 4 killed but the
//! `suspended` mark could not be written (the earlier spec is complete, so the lane is still
//! startable).
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the process facts and the sleep,
//! and writing its text and ledger rows, through the [`Ctx`] (thin-main PR 12). The leftover-tab
//! close (shared with the restart command, `shared.rs`) goes through the context's facts, so a test
//! sees the shell it would terminate and never kills one.

use super::ctx::Ctx;
use super::shared::close_leftover_tab;
use super::{identity_note, StopArgs};
use crate::spec::StopKind;
use crate::{lane_stop, relaunch, start, tab_close};
use std::process::ExitCode;

/// What one `stop` writes to the ledger about itself.
struct Ledger<'a> {
    args: &'a StopArgs,
    requested_by: String,
    caller_role: Option<String>,
    reason_for_log: String,
}

impl Ledger<'_> {
    fn write(&self, ctx: &mut Ctx, model: Option<&str>, outcome: &str, acted: bool) {
        ctx.log_ledger(
            &self.requested_by,
            &self.args.role,
            "stop",
            self.caller_role.as_deref(),
            model,
            Some(&self.reason_for_log),
            outcome,
            acted,
        );
    }
}

pub(super) fn run(ctx: &mut Ctx, args: StopArgs) -> ExitCode {
    let facts = ctx.facts();
    let claude_pid = ctx.caller_claude_pid();
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = if args.target_self {
        "self".to_string()
    } else {
        caller_role
            .clone()
            .unwrap_or_else(|| "unverified".to_string())
    };
    let reason_for_log: String = args
        .reason
        .trim()
        .chars()
        .take(start::REASON_MAX_CHARS)
        .collect();
    let ledger = Ledger {
        args: &args,
        requested_by: requested_by.clone(),
        caller_role,
        reason_for_log,
    };
    // Hard unless asked otherwise; a lane stopping itself is soft (board 216), and `--self --hard`
    // is refused by `plan_stop`.
    let kind = args.kind.unwrap_or(if args.target_self {
        StopKind::Soft
    } else {
        StopKind::Hard
    });
    let req = lane_stop::StopRequest {
        role: &args.role,
        reason: &args.reason,
        self_stop: args.target_self,
        caller_claude_pid: claude_pid,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
        kind,
    };
    let state = ctx.state_dir.clone();
    let plan = match lane_stop::plan_stop(
        &state,
        &req,
        caller,
        &*facts,
        &relaunch::policy_default_model(&state),
    ) {
        Ok(p) => p,
        Err(refusal) => {
            ctx.errln(&format!("lane-restart stop: refused - {refusal}"));
            ledger.write(ctx, None, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    };
    let pin_note = plan
        .pinned_from
        .as_deref()
        .map(|was| format!(" model-pinned:{was}"))
        .unwrap_or_default();
    if !plan.will_act {
        ctx.outln(&format!(
            "lane-restart stop: DRY RUN - would write the launch spec for {} (model {}, HANDOFF {}), \
             then kill pid {} and mark the spec suspended{pin_note}",
            plan.state.role, plan.stopping.model, plan.handoff_path, plan.state.pid
        ));
        ledger.write(
            ctx,
            Some(&plan.stopping.model),
            &format!("dry run - no action taken{pin_note}"),
            false,
        );
        return ExitCode::SUCCESS;
    }
    ctx.errln(&format!(
        "lane-restart stop: writing the launch spec, then stopping {} (pid {})...",
        plan.state.role, plan.state.pid
    ));
    // Captured while claude is still alive: after the kill its parent link is gone.
    let leftover_shell = tab_close::capture(&*facts, plan.state.pid, &plan.identity);
    let sleeper = ctx.sleeper();
    let result = lane_stop::execute_stop(
        &plan,
        &*facts,
        &crate::spec::DirSpecStore { dir: state.clone() },
        &mut |d| (*sleeper)(d),
    );
    match result {
        Ok(done) => {
            let warn = done
                .spec_warning
                .as_deref()
                .map(|w| format!("; the launch spec could not be marked suspended: {w}"))
                .unwrap_or_default();
            let exit = if done.spec_warning.is_some() {
                ExitCode::from(4)
            } else {
                ExitCode::SUCCESS
            };
            ledger.write(
                ctx,
                Some(&plan.stopping.model),
                &format!(
                    "stopped pid {} ({} stop); launch spec {}{pin_note}{warn}",
                    plan.state.pid,
                    kind.as_str(),
                    if done.spec_warning.is_some() {
                        "left as stopping (startable)"
                    } else {
                        "suspended"
                    }
                ),
                true,
            );
            // The tab closes LAST: on `--self` this process may share the tab's console.
            close_leftover_tab(
                ctx,
                &*facts,
                "stop",
                &requested_by,
                &args.role,
                leftover_shell,
                true,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
        Err(e) => {
            let note = match &e {
                lane_stop::StopError::Kill(inner) => identity_note(inner, plan.state.pid),
                lane_stop::StopError::SpecNotWritten(_) => String::new(),
            };
            ctx.errln(&format!("lane-restart stop: {e}{note}"));
            ledger.write(
                ctx,
                Some(&plan.stopping.model),
                &format!("action failed: {e}{pin_note}{note}"),
                false,
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
