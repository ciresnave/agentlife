// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart attach --role <name> --watch` - a read-only view of a running direct lane host
//! (`watch.rs`). Interactive attach is not built.
//!
//! Moved out of `app.rs` unchanged apart from reading the state directory and writing its text,
//! the watched screen included, through the [`Ctx`] (thin-main PR 7). The screen is raw bytes, so
//! it goes through `Ctx`'s `io::Write`, never through a text print.

use super::ctx::Ctx;
use crate::cli;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, rest: &[String]) -> ExitCode {
    let (role, watch) = match cli::attach(rest) {
        Ok(a) => a,
        Err(e) => {
            ctx.errln(&format!("lane-restart attach: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let Some(role) = role.filter(|r| crate::relaunch::valid_identifier(r)) else {
        ctx.errln("lane-restart attach: --role <name> is required and must be a plain identifier");
        return ExitCode::FAILURE;
    };
    if !watch {
        ctx.errln(concat!(
            "lane-restart attach: --watch is required. Only read-only watching exists; ",
            "interactive attach is not built (it is not approved until a threat-model note ",
            "says how a same-user agent is kept off the socket)."
        ));
        return ExitCode::FAILURE;
    }
    #[cfg(unix)]
    {
        let path = crate::watch::socket_path(&ctx.state_dir, &role);
        ctx.errln(&format!(
            "lane-restart attach: watching {role} (read-only; the host closes this when it ends)"
        ));
        match crate::watch::connect_and_copy(&path, ctx) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                ctx.errln(&format!(
                    "lane-restart attach: {}: {e} (is a direct host running for {role}?)",
                    path.display()
                ));
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = role;
        ctx.errln(
            "lane-restart attach: not available on this OS (a direct lane host, and so its watch              socket, exists only off Windows)",
        );
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests;
