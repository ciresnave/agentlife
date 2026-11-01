// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart waits [--role <name>]` - prints the `WAIT:` lines of this lane's HANDOFF, soonest
//! first, so a starting lane can arm each (`waits.rs`). Exit 1 when a line is malformed.
//!
//! Moved out of `app.rs` unchanged apart from reading its inputs and writing its text through the
//! [`Ctx`] (thin-main PR 2).

use super::ctx::Ctx;
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, role_override: Option<String>) -> ExitCode {
    let cwd = match &ctx.cwd {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            let e = e.clone();
            ctx.errln(&format!(
                "lane-restart waits: could not read the current directory: {e}"
            ));
            return ExitCode::FAILURE;
        }
    };
    let role = role_override
        .or_else(|| ctx.lane_role.clone())
        .unwrap_or_else(|| {
            std::path::Path::new(&cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let Some(text) = super::handoff_text(&cwd, &role) else {
        ctx.outln(&format!(
            "no HANDOFF for {role} under {cwd}: nothing to arm"
        ));
        return ExitCode::SUCCESS;
    };
    let now = chrono::Utc::now();
    let report = crate::waits::report(&text, now);
    if report.is_empty() {
        ctx.outln(&format!(
            "no WAIT lines in the HANDOFF for {role}: nothing to arm"
        ));
        return ExitCode::SUCCESS;
    }
    ctx.out(&report);
    if crate::waits::refusal(&text, now).is_err() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::run_with;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    fn ok(code: ExitCode) -> bool {
        format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
    }
    fn failed(code: ExitCode) -> bool {
        format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
    }

    fn ctx_in(dir: &Path, lane_role: Option<&str>) -> Ctx {
        Ctx::capture(dir.join("state"), Ok(dir.to_path_buf()), lane_role)
    }

    fn args(rest: &[&str]) -> Vec<OsString> {
        std::iter::once("lane-restart")
            .chain(rest.iter().copied())
            .map(OsString::from)
            .collect()
    }

    const GOOD: &str =
        "WAIT: a merge | check: gh pr view 1 --json state,mergedAt | wake: 2099-01-01T00:00:00Z\n";

    #[test]
    fn no_handoff_and_no_wait_lines_say_so_and_succeed() {
        let d = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(d.path(), None);
        assert!(ok(run(&mut ctx, Some("r".into()))));
        let (out, err) = ctx.captured();
        assert!(out.starts_with("no HANDOFF for r under "), "{out}");
        assert!(out.ends_with(": nothing to arm\n"), "{out}");
        assert_eq!(err, "");

        std::fs::write(d.path().join("HANDOFF.md"), "# nothing\n").unwrap();
        let mut ctx = ctx_in(d.path(), None);
        assert!(ok(run(&mut ctx, Some("r".into()))));
        assert_eq!(
            ctx.captured().0,
            "no WAIT lines in the HANDOFF for r: nothing to arm\n"
        );
    }

    #[test]
    fn a_good_wait_is_printed_with_its_check_and_wake_and_succeeds() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("HANDOFF.md"), GOOD).unwrap();
        let mut ctx = ctx_in(d.path(), None);
        assert!(ok(run(&mut ctx, Some("r".into()))));
        let (out, _) = ctx.captured();
        assert!(out.contains("line 1: a merge\n"), "{out}");
        assert!(
            out.contains("  check: gh pr view 1 --json state,mergedAt\n"),
            "{out}"
        );
        assert!(out.contains("2099-01-01T00:00:00+00:00"), "{out}");
    }

    #[test]
    fn a_malformed_wait_is_named_and_fails() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("HANDOFF.md"),
            "WAIT: a merge | check: gh pr view 1\n",
        )
        .unwrap();
        let mut ctx = ctx_in(d.path(), None);
        assert!(failed(run(&mut ctx, Some("r".into()))));
        let (out, _) = ctx.captured();
        assert!(
            out.contains("line 1: MALFORMED: no `wake: <time>`"),
            "{out}"
        );
    }

    #[test]
    fn the_role_is_the_flag_then_lane_role_then_the_directory_leaf() {
        let d = tempfile::tempdir().unwrap();
        let leaf = d.path().file_name().unwrap().to_string_lossy().to_string();
        let say = |role: Option<&str>, lane_role: Option<&str>| {
            let mut ctx = ctx_in(d.path(), lane_role);
            assert!(ok(run(&mut ctx, role.map(str::to_string))));
            ctx.captured().0
        };
        assert!(say(Some("flag"), Some("env")).contains("HANDOFF for flag "));
        assert!(say(None, Some("env")).contains("HANDOFF for env "));
        assert!(say(None, None).contains(&format!("HANDOFF for {leaf} ")));
    }

    #[test]
    fn an_unreadable_working_directory_is_named_on_stderr_and_fails() {
        let mut ctx = Ctx::capture(PathBuf::from("s"), Err("boom".into()), None);
        assert!(failed(run(&mut ctx, None)));
        let (out, err) = ctx.captured();
        assert_eq!(out, "");
        assert_eq!(
            err,
            "lane-restart waits: could not read the current directory: boom\n"
        );
    }

    #[test]
    fn the_command_line_reaches_it_through_run_with_and_a_bad_flag_does_not() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("HANDOFF.md"), GOOD).unwrap();
        let mut ctx = ctx_in(d.path(), None);
        assert!(ok(run_with(&mut ctx, args(&["waits", "--role", "r"]))));
        assert!(ctx.captured().0.contains("line 1: a merge"));
        // The accepted call above is the control for this refusal: the same HANDOFF, a bad flag.
        let mut ctx = ctx_in(d.path(), None);
        assert!(failed(run_with(&mut ctx, args(&["waits", "--bogus"]))));
        assert_eq!(ctx.captured().0, "");
    }
}
