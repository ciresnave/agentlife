// SPDX-License-Identifier: MIT OR Apache-2.0

//! `models` through the `Ctx` seam: lane state files in a tempdir; the machine's are never read.

use super::*;
use crate::app::run_with;
use crate::testing::state_at;
use std::ffi::OsString;

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn write_lane(state: &std::path::Path, role: &str, model: Option<&str>) {
    let mut st = state_at(role, "s", "Stop");
    st.model = model.map(str::to_string);
    std::fs::write(
        state.join(format!("{role}.json")),
        serde_json::to_string(&st).unwrap(),
    )
    .unwrap();
}

fn ctx(state: &std::path::Path) -> Ctx {
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None)
}

#[test]
fn each_lane_is_listed_sorted_with_opus_flagged() {
    let d = tempfile::tempdir().unwrap();
    write_lane(d.path(), "b", Some("claude-opus-5-5"));
    write_lane(d.path(), "a", Some("claude-sonnet-5-5"));
    write_lane(d.path(), "c", None);
    let mut c = ctx(d.path());
    assert!(ok(run_with(&mut c, args(&["models"]))));
    let (out, err) = c.captured();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "a\tclaude-sonnet-5-5\nb\tclaude-opus-5-5  <-- OPUS\nc\t-\n"
    );
}

#[test]
fn no_lanes_prints_one_empty_line_and_succeeds() {
    let d = tempfile::tempdir().unwrap();
    let mut c = ctx(d.path());
    assert!(ok(run_with(&mut c, args(&["models"]))));
    assert_eq!(c.captured(), ("\n".to_string(), String::new()));
}

#[test]
fn a_spec_file_is_not_a_lane() {
    let d = tempfile::tempdir().unwrap();
    write_lane(d.path(), "a", Some("claude-sonnet-5-5"));
    std::fs::write(d.path().join("a.launch.json"), "{}").unwrap();
    let mut c = ctx(d.path());
    assert!(ok(run_with(&mut c, args(&["models"]))));
    assert_eq!(c.captured().0, "a\tclaude-sonnet-5-5\n");
}

#[test]
fn models_with_an_argument_is_not_the_report() {
    // It falls to the restart parser and fails there.
    let d = tempfile::tempdir().unwrap();
    write_lane(d.path(), "a", Some("claude-sonnet-5-5"));
    let mut c = ctx(d.path());
    assert!(!ok(run_with(&mut c, args(&["models", "extra"]))));
    assert!(!c.captured().0.contains("claude-sonnet"));
}
