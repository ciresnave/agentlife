// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-new <role> <repo> [--model M] [--apply --plan <hash>]` - PM only
//! (docs/LANE-NEW.md). A dry run by default; `--apply` creates the repo, seeds it, writes the lane
//! files, provisions the Synapse identity and starts the lane, stopping at the first step whose
//! read-back fails.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the roots, GitHub and the host,
//! and writing its text and ledger row, through the [`Ctx`] (thin-main PR 10).

use super::ctx::Ctx;
use crate::start;
use std::process::ExitCode;

pub(super) fn run(
    ctx: &mut Ctx,
    role: String,
    repo: String,
    model: String,
    apply: bool,
    plan: Option<String>,
) -> ExitCode {
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may create a lane; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may create a lane, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart lane-new: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-new",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let roots = match ctx.lane_new_roots() {
        Ok(r) => r,
        Err(e) => {
            ctx.errln(&format!("lane-restart lane-new: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // The dry run also reads GitHub (repo visibility, main's head and files), the clone directory and
    // the disk; it creates, clones, provisions and starts nothing. `--apply` does all of it, step by
    // step, each read back (docs/LANE-NEW.md).
    let vcs = ctx.lane_new_vcs();
    let host = ctx.lane_new_host(&roots);
    // `--apply` goes through `run_apply`, which refuses unless `--plan` is the hash a dry run of exactly
    // these arguments printed: a typo'd role or repo cannot be applied without being planned first.
    let report = if apply {
        crate::lane_new::run_apply(
            &roots,
            vcs.as_deref(),
            host.as_deref(),
            &role,
            &repo,
            &model,
            plan.as_deref(),
            chrono::Utc::now(),
        )
    } else {
        crate::lane_new::run_full(
            &roots,
            vcs.as_deref(),
            host.as_deref(),
            &role,
            &repo,
            &model,
            false,
            chrono::Utc::now(),
        )
    };
    ctx.out(&report.render(apply));
    if apply {
        let steps: Vec<String> = report
            .rows
            .iter()
            .map(|r| format!("{}:{:?}", r.n, r.status))
            .collect();
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-new",
            caller_role.as_deref(),
            Some(&model),
            Some(&format!("repo {repo}")),
            &format!(
                "{}: {}",
                if report.failed() { "failed" } else { "created" },
                steps.join(" ")
            ),
            report
                .rows
                .iter()
                .any(|r| r.status == crate::lane_new::Status::Ok),
        );
    }
    if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
