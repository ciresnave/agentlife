// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `lane-restart` command line driven IN-PROCESS through `lane_restart::cli::run`, which is all
//! the binary is (CireSnave 2026-11-01: "any Rust binary should be a wrapper around its library so
//! that the library can be used for testing all internal functionality").
//!
//! Limits, said once: `run` prints with `println!`/`eprintln!`, so these tests read the EXIT CODE
//! and the files a command writes, not its text; and the state directory is the tool's fixed
//! machine location (`lane_restart::state_dir()`), so no test here writes to it or reads a
//! fixture from it. A `status` or `waits-poll` test on a fixture state directory needs a seam for
//! the directory and the output, which is a design call (see the PR).

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Mutex;

/// `run` reads the process's working directory (`assert-idle`, `waits`), which is process-global.
static CWD: Mutex<()> = Mutex::new(());

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// `ExitCode` has no `PartialEq`; its `Debug` form is the only comparison the standard library
/// offers, and it is stable for a given platform.
fn is_success(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn is_failure(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

/// Runs `rest` with `dir` as the working directory, restoring the old one afterwards.
fn run_in(dir: &std::path::Path, rest: &[&str]) -> ExitCode {
    let _guard = CWD.lock().unwrap_or_else(|e| e.into_inner());
    let before = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let code = lane_restart::cli::run(args(rest));
    std::env::set_current_dir(before).unwrap();
    code
}

#[test]
fn help_and_version_succeed_and_an_unknown_flag_or_command_is_exit_one() {
    assert!(is_success(lane_restart::cli::run(args(&["--help"]))));
    assert!(is_success(lane_restart::cli::run(args(&["-h"]))));
    assert!(is_success(lane_restart::cli::run(args(&["--version"]))));
    assert!(is_success(lane_restart::cli::run(args(&["-V"]))));
    // Not 2: 2 means "relaunched, awaiting a human at the dialog".
    assert!(is_failure(lane_restart::cli::run(args(&[
        "--no-such-flag"
    ]))));
    // The control that makes the line above mean something: the same call shape, accepted.
    assert!(!is_failure(lane_restart::cli::run(args(&["--help"]))));
}

#[test]
fn a_subcommand_with_a_bad_argument_is_exit_one_without_touching_anything() {
    for rest in [
        &["waits", "--no-such-flag"][..],
        &["waits-poll", "--no-such-flag"][..],
        &["assert-idle", "--no-such-flag"][..],
    ] {
        assert!(is_failure(lane_restart::cli::run(args(rest))), "{rest:?}");
    }
}

fn handoff(dir: &std::path::Path, body: &str) {
    std::fs::write(dir.join("HANDOFF.md"), body).unwrap();
}

const GOOD_WAIT: &str =
    "WAIT: a merge | check: gh pr view 1 --json state,mergedAt | wake: 2099-01-01T00:00:00Z\n";

#[test]
fn waits_prints_a_good_handoff_and_fails_a_malformed_one() {
    let d = tempfile::tempdir().unwrap();
    // No HANDOFF at all, a HANDOFF with no waits, and a good wait: all exit 0.
    assert!(is_success(run_in(d.path(), &["waits", "--role", "r"])));
    handoff(d.path(), "# nothing to wait for\n");
    assert!(is_success(run_in(d.path(), &["waits", "--role", "r"])));
    handoff(d.path(), GOOD_WAIT);
    assert!(is_success(run_in(d.path(), &["waits", "--role", "r"])));
    // A wait with no wake time is malformed: exit 1.
    handoff(
        d.path(),
        "WAIT: a merge | check: gh pr view 1 --json state,mergedAt\n",
    );
    assert!(is_failure(run_in(d.path(), &["waits", "--role", "r"])));
}

#[test]
fn assert_idle_refuses_a_malformed_wait_before_anything_is_recorded() {
    let d = tempfile::tempdir().unwrap();
    // A role no lane has, so a stray state file would be ours and nothing else's.
    let role = format!("run-in-process-{}", std::process::id());
    let state_file = lane_restart::state_dir().join(format!("{role}.json"));
    assert!(!state_file.exists(), "precondition: no such lane state");

    handoff(d.path(), "WAIT: a merge | check: x\n");
    let code = run_in(d.path(), &["assert-idle", "--role", &role]);
    assert!(is_failure(code), "a malformed wait is refused: {code:?}");
    assert!(
        !state_file.exists(),
        "a refused assert-idle must record nothing"
    );

    // Without the bad line the command goes on to the state lookup, which also fails for a role
    // that never started a session, and also records nothing: so the exit code alone cannot tell
    // the two refusals apart (no output capture; see the module comment), but the file can still
    // prove nothing was written either way.
    handoff(d.path(), GOOD_WAIT);
    let code = run_in(d.path(), &["assert-idle", "--role", &role]);
    assert!(is_failure(code), "{code:?}");
    assert!(!state_file.exists());
}
