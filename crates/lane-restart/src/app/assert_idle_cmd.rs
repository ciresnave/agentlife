// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart assert-idle [--role <name>]` - run by a lane itself, right after writing HANDOFF.
//! Separate from `cli::restart` for the same reason `state` is: a positional subcommand, not the
//! restart CLI's own flags.
//!
//! Moved out of `app.rs` unchanged apart from reading its inputs and writing its text through the
//! [`Ctx`] (thin-main PR 2). The process-table walk (`RealParentProcess`) is still the real one.

use super::ctx::Ctx;
use crate::lane_state_writer::{self, RealParentProcess};
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, role_override: Option<String>) -> ExitCode {
    let my_pid = std::process::id();
    let lane_role_env = role_override.or_else(|| ctx.lane_role.clone());
    let lint_role = lane_role_env.clone();
    let cwd = match &ctx.cwd {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            let e = e.clone();
            ctx.errln(&format!(
                "lane-restart assert-idle: could not read the current directory: {e}"
            ));
            return ExitCode::FAILURE;
        }
    };
    // The role the lint and the waits read the HANDOFF for: --role, then LANE_ROLE, then the cwd's leaf.
    let role = lint_role.unwrap_or_else(|| {
        std::path::Path::new(&cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    // A wait that cannot be woken is refused BEFORE anything is recorded: a refused assert-idle
    // must not leave the lane looking idle (waits.rs).
    let mut wait_warnings = Vec::new();
    if let Some(text) = super::handoff_text(&cwd, &role) {
        match crate::waits::refusal(&text, chrono::Utc::now()) {
            Ok(w) => wait_warnings = w,
            Err(e) => {
                ctx.errln(&format!("lane-restart assert-idle: {e}"));
                return ExitCode::FAILURE;
            }
        }
    }
    match lane_state_writer::run_assert_idle(
        &ctx.state_dir,
        my_pid,
        &RealParentProcess::new(),
        lane_role_env.as_deref(),
        &cwd,
    ) {
        Ok(()) => {
            for w in &wait_warnings {
                ctx.errln(&format!("lane-restart assert-idle: WARNING: {w}"));
            }
            // A warning, never a refusal: a restart is mandatory at a task boundary.
            if let Some(w) = crate::handoff_lint::check(&cwd, &role) {
                ctx.errln(&format!("lane-restart assert-idle: {w}"));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            ctx.errln(&format!("lane-restart assert-idle: {e}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::run_with;
    use std::ffi::OsString;
    use std::path::Path;

    fn failed(code: ExitCode) -> bool {
        format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
    }

    fn ctx_in(dir: &Path) -> Ctx {
        // A state directory of the test's own: never the machine's.
        Ctx::capture(dir.join("state"), Ok(dir.to_path_buf()), None)
    }

    fn args(rest: &[&str]) -> Vec<OsString> {
        std::iter::once("lane-restart")
            .chain(rest.iter().copied())
            .map(OsString::from)
            .collect()
    }

    const BAD_WAIT: &str = "WAIT: a merge | check: gh pr view 1 --json state,mergedAt\n";
    const GOOD_WAIT: &str =
        "WAIT: a merge | check: gh pr view 1 --json state,mergedAt | wake: 2099-01-01T00:00:00Z\n";

    #[test]
    fn a_malformed_wait_is_refused_by_name_and_nothing_is_recorded() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("HANDOFF.md"), BAD_WAIT).unwrap();
        let mut ctx = ctx_in(d.path());
        assert!(failed(run(&mut ctx, Some("r".into()))));
        let (out, err) = ctx.captured();
        assert_eq!(out, "");
        assert!(
            err.starts_with(
                "lane-restart assert-idle: HANDOFF has 1 wait line(s) that cannot be woken"
            ),
            "{err}"
        );
        assert!(err.contains("line 1: no `wake: <time>`"), "{err}");
        // Refused before the state directory was touched at all.
        assert!(!d.path().join("state").exists());
    }

    #[test]
    fn a_good_handoff_goes_on_to_the_state_lookup_which_is_a_different_refusal() {
        // The control that makes the test above mean something: the same command with a valid wait
        // is not refused for its waits; for a role that never recorded a session it stops at the
        // state lookup, and says so in different words.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("HANDOFF.md"), GOOD_WAIT).unwrap();
        let mut ctx = ctx_in(d.path());
        assert!(failed(run(&mut ctx, Some("r".into()))));
        let (_, err) = ctx.captured();
        assert!(err.contains("no state file"), "{err}");
        assert!(!err.contains("cannot be woken"), "{err}");
        assert!(!d.path().join("state").join("r.json").exists());
    }

    #[test]
    fn an_unreadable_working_directory_is_named_on_stderr_and_fails() {
        let mut ctx = Ctx::capture(d_path(), Err("boom".into()), None);
        assert!(failed(run(&mut ctx, None)));
        assert_eq!(
            ctx.captured().1,
            "lane-restart assert-idle: could not read the current directory: boom\n"
        );
    }

    fn d_path() -> std::path::PathBuf {
        std::path::PathBuf::from("s")
    }

    #[test]
    fn the_command_line_reaches_it_through_run_with_and_a_bad_flag_does_not() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("HANDOFF.md"), BAD_WAIT).unwrap();
        let mut ctx = ctx_in(d.path());
        assert!(failed(run_with(
            &mut ctx,
            args(&["assert-idle", "--role", "r"])
        )));
        assert!(ctx.captured().1.contains("cannot be woken"));
        let mut ctx = ctx_in(d.path());
        assert!(failed(run_with(
            &mut ctx,
            args(&["assert-idle", "--bogus"])
        )));
        let (out, err) = ctx.captured();
        assert_eq!(out, "");
        assert!(err.starts_with("lane-restart assert-idle: "), "{err}");
        assert!(!err.contains("cannot be woken"), "{err}");
    }
}
