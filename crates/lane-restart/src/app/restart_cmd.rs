// SPDX-License-Identifier: MIT OR Apache-2.0

//! The default command: `lane-restart (--role <name> | --self) [--dry-run] [--yes]`. Restart a
//! lane: the target is decided (`authorize::decide`), then either a dry run is described or the
//! lane's claude is killed and a fresh session relaunched from its launch spec, and the tab a
//! hand-started lane's shell keeps open is closed last.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the claude pid, the working
//! directory, `LANE_ROLE`, the process facts, the launch spawner, the sleep and the Opus permit,
//! and writing its text and ledger rows, through the [`Ctx`] (thin-main PR 13).

use super::ctx::Ctx;
use super::shared::{close_leftover_tab, disk_gate, print_status_json};
use super::{identity_note, Args};
use crate::authorize::{self, Target};
use crate::{lane_stop, relaunch, tab_close};
use std::process::ExitCode;

/// PM finding, 2026-09-19 (real-restart-2): exit 0 is indistinguishable from `Relaunched` for
/// anything scripting against this tool - a genuinely different, real outcome needs a genuinely
/// different exit code. `AwaitingConfirmation` is real, ACTED work (§5) but not the same as
/// confirmed progress, so it gets its own code rather than sharing `Relaunched`'s 0 or `Err`'s 1.
pub(super) fn relaunch_exit_code(outcome: relaunch::RelaunchOutcome) -> ExitCode {
    match outcome {
        relaunch::RelaunchOutcome::Relaunched => ExitCode::SUCCESS,
        relaunch::RelaunchOutcome::AwaitingConfirmation => ExitCode::from(2),
    }
}

/// A restart-path ledger line (`kind: restart`), with `caller_role` set whenever `requested_by` is
/// a verified role (that is, neither the `--self` label nor "unverified").
fn log_outcome(ctx: &mut Ctx, requested_by: &str, role: &str, outcome: &str, acted: bool) {
    let caller_role =
        (requested_by != "self" && requested_by != "unverified").then_some(requested_by);
    ctx.log_ledger(
        requested_by,
        role,
        "restart",
        caller_role,
        None,
        None,
        outcome,
        acted,
    );
}

pub(super) fn run(ctx: &mut Ctx, args: Args) -> ExitCode {
    // A bare `--self` (the documented form): the role is the caller's own, found the way
    // `assert-idle` finds it. An explicit `--role` is never second-guessed here.
    let role = match args.role.clone() {
        Some(r) => r,
        None => {
            let cwd = ctx
                .cwd
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let claude_pid = ctx.caller_claude_pid();
            match crate::spec::resolve_self_role(
                &ctx.state_dir,
                ctx.lane_role.as_deref(),
                &cwd,
                claude_pid,
            ) {
                Ok(r) => r,
                Err(e) => {
                    ctx.errln(&format!("lane-restart: --self without --role: {e}"));
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let target = if args.target_self {
        Target::Myself {
            role: role.clone(),
            // The same ancestry walk the hooks use to record `pid`: this
            // process <- shell(s) <- claude. `decide` compares it with the
            // state file's pid, so `--self` is a claim it can check.
            caller_pid: ctx.caller_claude_pid(),
        }
    } else {
        Target::Other { role: role.clone() }
    };

    let facts = ctx.facts();
    // 14.7 / 14.8: `requested_by` is the VERIFIED caller (the role whose claude session ran
    // this), not a label chosen by the absence of `--self`. A caller that cannot be verified is
    // recorded as "unverified"; a restart of another lane is still not refused on it (the
    // `--yes` + idle checks are unchanged), so the ledger is where a non-PM caller shows up.
    let caller = (!args.target_self).then(|| ctx.verified_caller());
    let caller_role = caller.as_ref().and_then(|c| c.as_ref().ok().cloned());
    let requested_by: &str = if args.target_self {
        "self"
    } else {
        caller_role.as_deref().unwrap_or("unverified")
    };
    // RESTART-TOOL-DESIGN.md 14.7 / 14.8, the PM's ruling of 2026-10-11: restarting ANOTHER lane
    // is the PM session's alone, the same rule as `stop`. Refused (and logged) before anything
    // about the target is read, even for a dry run.
    if let Some(caller) = caller {
        if let Err(refusal) = lane_stop::require_pm_for_other(false, caller) {
            ctx.errln(&format!("lane-restart: refused - {refusal}"));
            log_outcome(
                ctx,
                requested_by,
                &role,
                &format!("refused: {refusal}"),
                false,
            );
            return ExitCode::FAILURE;
        }
    }
    let req = authorize::Request {
        target,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
    };

    let state = ctx.state_dir.clone();
    let decision = authorize::decide(&req, &*facts, &state);
    // The target's own session and its host are what this restart replaces; any OTHER host or
    // claude of the role is a second session the state file may not show (2026-10-14).
    if let Ok(plan) = &decision {
        let names = crate::duplicate::role_names(&state, &role);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        if let Err(refusal) = crate::duplicate::check(&*facts, &role, &names, Some(plan.state.pid))
        {
            ctx.errln(&format!("lane-restart: refused - {refusal}"));
            log_outcome(
                ctx,
                requested_by,
                &role,
                &format!("refused: {refusal}"),
                false,
            );
            return ExitCode::FAILURE;
        }
    }
    match decision {
        Err(refusal) => {
            ctx.errln(&format!("lane-restart: refused - {refusal}"));
            log_outcome(
                ctx,
                requested_by,
                &role,
                &format!("refused: {refusal}"),
                false,
            );
            ExitCode::FAILURE
        }
        Ok(plan) if !plan.will_act => {
            match relaunch::describe_dry_run(&plan.state) {
                Ok(argv) => {
                    let cmdline: Vec<String> = argv
                        .iter()
                        .map(|a| {
                            if a.contains(' ') {
                                format!("{a:?}")
                            } else {
                                a.clone()
                            }
                        })
                        .collect();
                    ctx.outln(&format!(
                        "lane-restart: DRY RUN - would kill pid {} and relaunch a FRESH session \
                         via wt.exe (conhost.exe fallback), hosted by \
                         `lane-restart host --role {} -- {}`, in {}",
                        plan.state.pid,
                        plan.state.role,
                        cmdline.join(" "),
                        plan.state.cwd
                    ));
                }
                Err(e) => {
                    ctx.outln(&format!(
                        "lane-restart: DRY RUN - would kill pid {} but the relaunch command \
                         itself would be refused: {e}",
                        plan.state.pid
                    ));
                }
            }
            match tab_close::capture(&*facts, plan.state.pid, &plan.identity) {
                Ok(shell) => ctx.outln(&format!(
                    "lane-restart: DRY RUN - would then close the leftover tab by terminating \
                     {shell}, if it has no other children by then"
                )),
                Err(why) => ctx.outln(&format!(
                    "lane-restart: DRY RUN - would leave the tab's shell alone: {why}"
                )),
            }
            log_outcome(ctx, requested_by, &role, "dry run - no action taken", false);
            ExitCode::SUCCESS
        }
        Ok(plan) => {
            // Before the kill: after it, a refusal would leave a lane dead and not relaunched.
            if let Err(refusal) = disk_gate(ctx, "restart") {
                log_outcome(
                    ctx,
                    requested_by,
                    &role,
                    &format!("refused: {refusal}"),
                    false,
                );
                return ExitCode::FAILURE;
            }
            ctx.errln(&format!(
                "lane-restart: killing pid {} and relaunching a fresh session...",
                plan.state.pid
            ));
            let state_reader = relaunch::RealStateReader { state_dir: &state };
            let role_for_notice = plan.state.role.clone();
            let spawn = ctx.launch_spawn();
            let sleeper = ctx.sleeper();
            let permit = ctx.opus_permit();
            // Captured while claude is still alive, moments before `kill_and_relaunch` kills it:
            // after the kill, claude's parent link is gone with it.
            let leftover_shell = tab_close::capture(&*facts, plan.state.pid, &plan.identity);
            let relaunch_result = {
                let mut on_awaiting = || {
                    // PM finding, 2026-09-18 (fifth real-restart retest): printed immediately,
                    // the moment this is first detected, so whatever reads this stream (the PM
                    // lane, today) can notify CireSnave promptly that the dev-channels dialog
                    // needs a human.
                    print_status_json(ctx, "awaiting_confirmation", &role_for_notice);
                };
                relaunch::kill_and_relaunch_persisting(
                    &*facts,
                    &state_reader,
                    &plan.state,
                    &plan.identity,
                    &relaunch::policy_default_model(&state),
                    permit.as_ref(),
                    &crate::spec::DirSpecStore { dir: state.clone() },
                    &*spawn,
                    &mut |d| (*sleeper)(d),
                    &mut on_awaiting,
                )
            };
            // The tab closes LAST: on `--self`, this process may share the tab's console and go
            // with it, so everything else is already logged by then.
            let relaunch_acted = relaunch_result.is_ok();
            let exit = match relaunch_result {
                Ok(outcome @ relaunch::RelaunchOutcome::Relaunched) => {
                    log_outcome(
                        ctx,
                        requested_by,
                        &role,
                        &format!(
                            "killed pid {} and relaunched a fresh session (continuity via HANDOFF only)",
                            plan.state.pid
                        ),
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Ok(outcome @ relaunch::RelaunchOutcome::AwaitingConfirmation) => {
                    log_outcome(
                        ctx,
                        requested_by,
                        &role,
                        "awaiting human confirmation at the dev-channels dialog",
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Err(e) => {
                    let note = identity_note(&e, plan.state.pid);
                    ctx.errln(&format!("lane-restart: {e}{note}"));
                    log_outcome(
                        ctx,
                        requested_by,
                        &role,
                        &format!("action failed: {e}{note}"),
                        false,
                    );
                    ExitCode::FAILURE
                }
            };
            close_leftover_tab(
                ctx,
                &*facts,
                "restart",
                requested_by,
                &role,
                leftover_shell,
                relaunch_acted,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
    }
}

#[cfg(test)]
mod tests;
