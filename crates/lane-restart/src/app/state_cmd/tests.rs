// SPDX-License-Identifier: MIT OR Apache-2.0

//! `state` through the `Ctx` seam: the hook's stdin is a string, its process-parent walk is a fake,
//! and the lane state it writes goes to a tempdir (a real `state <event>` writes lane state, so
//! nothing here may reach the machine's state directory).

use super::super::test_fixtures::UnderClaude;
use super::*;
use crate::app::run_with;
use crate::lane_state_writer::ParentProcess;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn failed(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

fn hook_json(event: &str) -> String {
    format!(r#"{{"hook_event_name":"{event}","session_id":"s1","cwd":"C:/Projects/x"}}"#)
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

fn ctx(state: &Path, role: Option<&str>, stdin: &str) -> Ctx {
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), role)
        .with_parents(UnderClaude)
        .with_stdin(stdin)
}

/// A walk that finds the `claude` ancestor, but only after `delay`: a slow process scan.
struct Slow(Duration);
impl ParentProcess for Slow {
    fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
        std::thread::sleep(self.0);
        UnderClaude.parent_of(pid)
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}

/// A walk with no `claude` anywhere above this process.
struct NoClaude;
impl ParentProcess for NoClaude {
    fn parent_of(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}

#[test]
fn a_session_start_writes_the_lane_state_under_the_role_in_the_state_dir() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, Some("alpha"), &hook_json("SessionStart"));
    assert!(ok(run(&mut c, "SessionStart")));
    let (out, err) = c.captured();
    assert_eq!((out.as_str(), err.as_str()), ("", ""));
    let written = crate::state::load(&state, "alpha").expect("alpha.json was written");
    assert_eq!(written.role, "alpha");
    assert_eq!(written.session_id, "s1");
    assert_eq!(
        written.pid, 55,
        "the verified claude ancestor, not this test"
    );
    assert!(!state.join("alpha.lock").exists(), "the lock is released");
}

#[test]
fn a_session_end_removes_the_lane_state() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut start = ctx(&state, Some("alpha"), &hook_json("SessionStart"));
    assert!(ok(run(&mut start, "SessionStart")));
    assert!(state.join("alpha.json").exists());
    let mut end = ctx(&state, Some("alpha"), &hook_json("SessionEnd"));
    assert!(ok(run(&mut end, "SessionEnd")));
    assert!(!state.join("alpha.json").exists());
}

#[test]
fn the_role_comes_from_the_context_not_the_process_environment() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, Some("beta"), &hook_json("SessionStart"));
    assert!(ok(run(&mut c, "SessionStart")));
    assert!(state.join("beta.json").exists());
    assert!(!state.join("alpha.json").exists());
}

#[test]
fn unparseable_input_fails_on_stderr_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, Some("alpha"), "not json");
    assert!(failed(run(&mut c, "SessionStart")));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(
        err.starts_with("lane-restart state SessionStart: could not parse hook input"),
        "{err}"
    );
    assert!(!state.join("alpha.json").exists());
}

#[test]
fn no_claude_ancestor_fails_naming_the_event_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = Ctx::capture(state.clone(), Ok(state.clone()), Some("alpha"))
        .with_parents(NoClaude)
        .with_stdin(&hook_json("Stop"));
    assert!(failed(run(&mut c, "Stop")));
    let err = c.captured().1;
    assert!(err.starts_with("lane-restart state Stop: "), "{err}");
    assert!(!state.join("alpha.json").exists());
}

#[test]
fn a_slow_hook_says_so_on_stderr_and_still_succeeds() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = Ctx::capture(state.clone(), Ok(state.clone()), Some("alpha"))
        .with_parents(Slow(Duration::from_millis(520)))
        .with_stdin(&hook_json("SessionStart"));
    assert!(ok(run(&mut c, "SessionStart")));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(
        err.starts_with("lane-restart state SessionStart: slow, "),
        "{err}"
    );
    assert!(err.contains("(0 full process scan(s))"), "{err}");
    assert!(state.join("alpha.json").exists());
}

#[test]
fn a_fast_hook_prints_nothing() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, Some("alpha"), &hook_json("Stop"));
    assert!(ok(run(&mut c, "Stop")));
    assert_eq!(c.captured(), (String::new(), String::new()));
}

#[test]
fn state_with_exactly_one_event_dispatches_to_the_hook() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    let mut c = ctx(&state, Some("alpha"), &hook_json("SessionStart"));
    assert!(ok(run_with(&mut c, args(&["state", "SessionStart"]))));
    assert!(state.join("alpha.json").exists());
}

#[test]
fn state_alone_and_state_with_two_arguments_are_not_the_hook() {
    // They fall to the restart parser and fail there; nothing is read or written.
    for rest in [&["state"][..], &["state", "a", "b"][..]] {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let mut c = ctx(&state, Some("alpha"), &hook_json("SessionStart"));
        assert!(failed(run_with(&mut c, args(rest))), "{rest:?}");
        assert!(!state.join("alpha.json").exists(), "{rest:?}");
    }
}

#[test]
fn the_real_context_builds_no_parent_walk_until_the_hook_asks() {
    let c = Ctx::real();
    assert_eq!(c.parent_full_scans(), 0);
}
