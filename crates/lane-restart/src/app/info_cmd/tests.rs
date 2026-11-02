// SPDX-License-Identifier: MIT OR Apache-2.0

//! `--help` and `--version` through the `Ctx` seam: the home directory is a tempdir (or none), so
//! `~/.overmind` on the machine is never read.

use super::*;
use crate::app::run_with;
use std::ffi::OsString;
use std::path::Path;

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn ctx(home: Option<&Path>) -> Ctx {
    let state = std::env::temp_dir().join("lane-restart-info-test-unused");
    Ctx::capture(state.clone(), Ok(state), None).with_home(home.map(Path::to_path_buf))
}

#[test]
fn help_prints_the_usage_text_in_either_spelling() {
    for flag in ["--help", "-h"] {
        let mut c = ctx(None);
        assert!(ok(run_with(&mut c, args(&[flag]))));
        let (out, err) = c.captured();
        assert_eq!(out, crate::app::USAGE);
        assert_eq!(err, "");
    }
}

#[test]
fn help_wins_over_everything_else_on_the_line() {
    let mut c = ctx(None);
    assert!(ok(run_with(&mut c, args(&["stop", "--bogus", "--help"]))));
    assert_eq!(c.captured().0, crate::app::USAGE);
}

#[test]
fn version_with_no_home_says_so() {
    for flag in ["--version", "-V"] {
        let mut c = ctx(None);
        assert!(ok(run_with(&mut c, args(&[flag]))));
        assert_eq!(
            c.captured().0,
            format!(
                "lane-restart {}\nembedded approvals: none (by design - RESTART-TOOL-DESIGN.md §12.2)\napprovals source: no home directory\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }
}

#[test]
fn version_with_a_home_but_no_config_reports_it_is_not_configured() {
    let d = tempfile::tempdir().unwrap();
    let mut c = ctx(Some(d.path()));
    assert!(ok(run_with(&mut c, args(&["--version"]))));
    let out = c.captured().0;
    let last = out.lines().last().unwrap();
    assert!(last.starts_with("approvals source: "), "{out}");
    assert!(last.contains("lane-restart.json"), "{out}");
}

#[test]
fn version_with_a_config_names_the_source_and_the_notify_role() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join(".overmind");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lane-restart.json"),
        r#"{"approvals":{"repo":"owner/repo","path":".overmind/lane-restart/approvals"},"notify_role":"boss"}"#,
    )
    .unwrap();
    let mut c = ctx(Some(d.path()));
    assert!(ok(run_with(&mut c, args(&["--version"]))));
    let out = c.captured().0;
    assert!(
        out.ends_with(
            "approvals source: owner/repo:.overmind/lane-restart/approvals (default branch), notify role: boss\n"
        ),
        "{out}"
    );
}
