// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart install --from <exe> [--dry-run]` - PM only, even as a dry run: verifies a freshly
//! built `lane-restart` and swaps it in at the hooks directory, keeping the old one.
//!
//! Moved out of `app.rs` unchanged apart from reading the caller, the hooks directory and the
//! candidate probe, and writing its text and its `restart.log` lines, through the [`Ctx`] (thin-main
//! PR 8). This is the first MUTATING command to move, so the seam gained the shared pieces the
//! others need: `Ctx::verified_caller` and `Ctx::log_ledger`.

use super::ctx::Ctx;
use crate::start;
use std::path::PathBuf;
use std::process::ExitCode;

/// Really runs the candidate binary (`--version`, then `status`).
pub(super) struct RealProbe;

impl crate::install::Probe for RealProbe {
    fn version_output(&self, exe: &std::path::Path) -> Result<String, String> {
        let out = std::process::Command::new(exe)
            .arg("--version")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
        if !out.status.success() {
            return Err(format!("exited {}", out.status));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn status_ok(&self, exe: &std::path::Path) -> Result<(), String> {
        let out = std::process::Command::new(exe)
            .arg("status")
            .output()
            .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

pub(super) fn run(ctx: &mut Ctx, from: PathBuf, dry_run: bool) -> ExitCode {
    let caller = ctx.verified_caller();
    let caller_role = caller.as_ref().ok().cloned();
    let requested_by = caller_role
        .clone()
        .unwrap_or_else(|| "unverified".to_string());
    let role = crate::install::EXE_NAME;
    if caller_role.as_deref() != Some(start::PM_ROLE) {
        let why = match caller {
            Ok(r) => format!("only the PM session may install the binary; this is running under {r:?}"),
            Err(e) => format!(
                "only the PM session may install the binary, and the caller could not be verified: {e}"
            ),
        };
        ctx.errln(&format!("lane-restart install: refused - {why}"));
        ctx.log_ledger(
            &requested_by,
            role,
            "install",
            caller_role.as_deref(),
            None,
            None,
            &format!("refused: {why}"),
            false,
        );
        return ExitCode::FAILURE;
    }
    let hooks = ctx.hooks_dir();
    let probe = ctx.probe();
    match crate::install::install(&hooks, &from, &*probe, dry_run) {
        Ok(done) => {
            let kept = done
                .kept_as
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "nothing was installed before".to_string());
            if dry_run {
                ctx.outln(&format!(
                    "DRY RUN - nothing written. {} verified: lane-restart {}, sha256 {}; the installed exe would be kept as {kept}",
                    from.display(),
                    done.version,
                    done.sha256
                ));
            } else {
                ctx.log_ledger(
                    &requested_by,
                    role,
                    "install",
                    caller_role.as_deref(),
                    None,
                    None,
                    &format!(
                        "installed {} sha256 {} from {}; previous kept as {kept}",
                        done.version,
                        done.sha256,
                        from.display()
                    ),
                    true,
                );
                ctx.outln(&format!(
                    "INSTALLED lane-restart {} at {}; sha256 {}; the previous exe is {kept}; {} rewritten",
                    done.version,
                    hooks.join(crate::install::EXE_NAME).display(),
                    done.sha256,
                    hooks.join(crate::install::SHA_NAME).display()
                ));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            ctx.errln(&format!("lane-restart install: {e}"));
            ctx.log_ledger(
                &requested_by,
                role,
                "install",
                caller_role.as_deref(),
                None,
                None,
                &format!("{e}"),
                false,
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
