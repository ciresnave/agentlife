// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart start --role R --reason <text> [--model sonnet|haiku] [--signal-id <id>]
//! [--dry-run] [--json]` - 14.5.
//!
//! A launcher caller (and only one) is also held to `restore.lock`, the caps and the memory floor
//! (`launcher_gate`); the PM's own start is not.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the process facts, the launch
//! spawner, the sleep, the environment and free memory, and writing its text and ledger rows,
//! through the [`Ctx`] (thin-main PR 11). The `finish`/`refuse` closures became methods of
//! [`Outcome`] taking the context, so the `awaiting` notice can borrow it too.

use super::ctx::Ctx;
use super::shared::{disk_gate, print_status_json};
use super::StartArgs;
use crate::{relaunch, start};
use std::process::ExitCode;

/// What one `start` writes about itself: one ledger line and, under `--json`, one stdout line, from
/// one `(outcome, reason)` so the two cannot disagree.
struct Outcome<'a> {
    args: &'a StartArgs,
    requested_by: String,
    caller_role: Option<String>,
    reason_for_log: String,
}

impl Outcome<'_> {
    /// `spawned` is whether a spawn was attempted: what the launcher caps count. `--json`:
    /// `{role,outcome,reason[,launch_to_live_secs]}`; the human lines stay on stderr and the exit
    /// codes do not change.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        ctx: &mut Ctx,
        model: Option<&str>,
        text: &str,
        acted: bool,
        outcome: &str,
        reason: &str,
        spawned: bool,
        secs: Option<u64>,
    ) {
        let result = format!("{outcome}:{reason}");
        ctx.log_ledger_start(
            &self.requested_by,
            &self.args.role,
            self.caller_role.as_deref(),
            model,
            Some(&self.reason_for_log),
            text,
            acted,
            self.args.signal_id.as_deref(),
            &result,
            spawned,
        );
        if self.args.json {
            let mut v = serde_json::json!({
                "role": self.args.role, "outcome": outcome, "reason": reason,
            });
            if let Some(s) = secs {
                v["launch_to_live_secs"] = s.into();
            }
            ctx.outln(&v.to_string());
        }
    }

    fn refuse(&self, ctx: &mut Ctx, refusal: &start::StartRefusal) -> ExitCode {
        ctx.errln(&format!("lane-restart start: refused - {refusal}"));
        let (o, r) = refusal.outcome();
        self.finish(
            ctx,
            None,
            &format!("refused: {refusal}"),
            false,
            o,
            r,
            false,
            None,
        );
        ExitCode::from(refusal.exit_code())
    }
}

pub(super) fn run(ctx: &mut Ctx, args: StartArgs) -> ExitCode {
    let facts = ctx.facts();
    let caller = ctx.verified_start_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let is_launcher = caller_role
        .as_deref()
        .is_some_and(start::is_launcher_caller);
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    // The ledger line names the signal this start answers, inside the 200-character reason cap.
    let reason_for_log: String = match args.signal_id.as_deref() {
        Some(s) => format!("{} [signal {s}]", args.reason.trim()),
        None => args.reason.trim().to_string(),
    }
    .chars()
    .take(start::REASON_MAX_CHARS)
    .collect();
    let out = Outcome {
        args: &args,
        requested_by,
        caller_role: caller_role.clone(),
        reason_for_log,
    };
    let req = start::StartRequest {
        role: &args.role,
        reason: &args.reason,
        model: args.model.as_deref(),
        signal_id: args.signal_id.as_deref(),
    };
    let state = ctx.state_dir.clone();
    // One starter per role from the check to the confirmed-live wait: the lock is taken before
    // `plan_start`, so its checks are the re-check after the lock. A dry run spawns nothing.
    let _start_lock = if args.dry_run {
        None
    } else {
        match start::take_start_lock(&state, &args.role) {
            Ok(lock) => lock,
            Err(refusal) => return out.refuse(ctx, &refusal),
        }
    };
    let plan = match start::plan_start(&state, &req, caller, &*facts) {
        Ok(p) => p,
        Err(refusal) => return out.refuse(ctx, &refusal),
    };
    if let Err(refusal) = disk_gate(ctx, "start") {
        out.finish(
            ctx,
            None,
            &format!("refused: {refusal}"),
            false,
            "failed",
            "disk-floor",
            false,
            None,
        );
        return ExitCode::FAILURE;
    }
    let pin_note = plan
        .pinned_from
        .as_deref()
        .map(|was| format!(" model-pinned:{was}"))
        .unwrap_or_default();
    if args.dry_run {
        let cmdline: Vec<String> = plan
            .argv
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
            "lane-restart start: DRY RUN - would launch {} via wt.exe (conhost.exe fallback), \
             hosted by `lane-restart host --role {} -- {}`, in {}{pin_note}",
            plan.launch.role,
            plan.launch.role,
            cmdline.join(" "),
            plan.launch.cwd
        ));
        out.finish(
            ctx,
            Some(&plan.model),
            &format!("dry run - no action taken{pin_note}"),
            false,
            "dry-run",
            "dry-run",
            false,
            None,
        );
        return ExitCode::SUCCESS;
    }
    // The launcher path's own gates, after every spec refusal (so `already-running` is still
    // `already-running` on a full machine) and before anything spawns. `restore.lock` is taken
    // first and held to the end, so two launcher starts cannot both read a count of 11 spawns.
    // A dry run took none of this and says so only by not reporting it.
    let _restore_lock = if is_launcher {
        let home = crate::launcher_gate::agentlife_home(ctx.env_var("AGENTLIFE_HOME").as_deref());
        let lock = match crate::launcher_gate::take_restore_lock(&home, &*facts) {
            Ok(l) => l,
            Err(refusal) => return out.refuse(ctx, &refusal),
        };
        let floor = crate::launcher_gate::resolve_floor_gb(
            &home.join("config.json"),
            ctx.env_var("AGENTLIFE_FREE_RAM_FLOOR_GB").as_deref(),
        );
        if let Err(refusal) = crate::launcher_gate::decide(
            crate::launcher_gate::load_rows(&state.join("restart.log")),
            &args.role,
            chrono::Utc::now(),
            floor,
            ctx.free_memory_gb(),
        ) {
            return out.refuse(ctx, &refusal);
        }
        Some(lock)
    } else {
        None
    };
    ctx.errln(&format!(
        "lane-restart start: launching {} on {}...",
        args.role, plan.model
    ));
    let state_reader = relaunch::RealStateReader { state_dir: &state };
    let role_for_notice = plan.launch.role.to_string();
    let spawn = ctx.launch_spawn();
    let sleeper = ctx.sleeper();
    let timing = ctx.liveness_timing();
    let mut on_awaiting = || print_status_json(ctx, "awaiting_confirmation", &role_for_notice);
    let result = start::execute_start(
        &plan,
        &*facts,
        &state_reader,
        &crate::spec::DirSpecStore { dir: state.clone() },
        &timing,
        &*spawn,
        &mut |d| (*sleeper)(d),
        &mut on_awaiting,
    );
    match result {
        Ok(done) => {
            let warn = done
                .spec_warning
                .as_deref()
                .map(|w| format!("; the launch spec could not be marked running: {w}"))
                .unwrap_or_default();
            // Seconds from spawn to confirmed progress, only when it was confirmed: an
            // awaiting-dialog wait is a person's time, not the launch's (spec C4 `launch_wait`).
            let (what, exit, secs, reason) = match done.outcome {
                relaunch::RelaunchOutcome::Relaunched => (
                    "started a fresh session (continuity via HANDOFF only)",
                    ExitCode::SUCCESS,
                    Some(done.launch_elapsed.as_secs()),
                    "started",
                ),
                relaunch::RelaunchOutcome::AwaitingConfirmation => (
                    "started; awaiting human confirmation at the dev-channels dialog",
                    ExitCode::from(2),
                    None,
                    "awaiting-dialog",
                ),
            };
            let live = secs
                .map(|s| format!(" launch-to-live:{s}s"))
                .unwrap_or_default();
            out.finish(
                ctx,
                Some(&plan.model),
                &format!("{what}{pin_note}{warn}{live}"),
                true,
                "started",
                reason,
                true,
                secs,
            );
            exit
        }
        Err(e) => {
            ctx.errln(&format!("lane-restart start: {e}"));
            out.finish(
                ctx,
                Some(&plan.model),
                &format!("action failed: {e}{pin_note}"),
                false,
                "failed",
                "spawn-failed",
                true,
                None,
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
