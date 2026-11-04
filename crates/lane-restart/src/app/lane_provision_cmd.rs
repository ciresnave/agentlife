// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-provision <role> [--apply --plan <hash>]` - PM only. A dry run by default;
//! `--apply` needs the hash the dry run of the same role printed. See `lane_provision`.

use super::ctx::Ctx;
use crate::start;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, role: String, apply: bool, plan: Option<String>) -> ExitCode {
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!(
                "only the PM session may provision a lane; this is running under {r:?}"
            ),
            Err(e) => format!(
                "only the PM session may provision a lane, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart lane-provision: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-provision",
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
            ctx.errln(&format!("lane-restart lane-provision: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let host = ctx.lane_new_host(&roots);
    let now = chrono::Utc::now();
    let report = if apply {
        crate::lane_provision::run_apply(&roots, host.as_deref(), &role, plan.as_deref(), now)
    } else {
        crate::lane_provision::run(&roots, host.as_deref(), &role, false, now)
    };
    ctx.out(&report.render_as("lane-provision", "the same role", apply));
    if apply {
        let steps: Vec<String> = report
            .rows
            .iter()
            .map(|r| format!("{}:{:?}", r.n, r.status))
            .collect();
        ctx.log_ledger(
            &requested_by,
            &role,
            "lane-provision",
            caller_role.as_deref(),
            None,
            None,
            &format!(
                "{}: {}",
                if report.failed() {
                    "failed"
                } else {
                    "provisioned"
                },
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
