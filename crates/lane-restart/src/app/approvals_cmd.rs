// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart approvals [--role <name>]` - fetches the configured approvals exactly as `host`
//! would and prints every active one (id, content hash, roles, expiry) and every refused one with
//! its reason. With `--role`, marks which apply to that role right now. Exit 1 if nothing could be
//! loaded at all.
//!
//! Moved out of `app.rs` unchanged apart from reading the home directory and the approvals reader,
//! and writing its text, through the [`Ctx`] (thin-main PR 6).

use super::ctx::Ctx;
use crate::approvals;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, role: Option<String>) -> ExitCode {
    let Some(home) = ctx.home() else {
        ctx.errln("lane-restart approvals: no home directory");
        return ExitCode::FAILURE;
    };
    let reader = ctx.repo_reader(&home);
    let (_, loaded) = approvals::load_configured(&home, &*reader);
    ctx.outln(&format!("approvals: {}", loaded.summary()));
    let now = chrono::Utc::now();
    for (h, hash) in &loaded.handlers {
        let applies = match &role {
            Some(r) if crate::handlers::is_active(h, r, now) => format!(" [applies to {r}]"),
            Some(r) => format!(" [NOT active for {r}]"),
            None => String::new(),
        };
        let expiry = h
            .expires_at
            .map_or_else(|| "never".to_string(), |e| e.to_rfc3339());
        ctx.outln(&format!(
            "  active {} {hash} roles={:?} expires={expiry}{applies}",
            h.id, h.scope.roles
        ));
    }
    for (name, reason) in &loaded.refused {
        ctx.outln(&format!("  REFUSED {name}: {reason}"));
    }
    if loaded.error.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
