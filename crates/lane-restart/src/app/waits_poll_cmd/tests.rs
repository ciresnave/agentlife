// SPDX-License-Identifier: MIT OR Apache-2.0

//! `waits-poll` through the `Ctx` seam: a fixture state directory with a LIVE lane (fake process
//! facts), a fake check runner, a capturing sender, and the text the command printed.

use super::super::test_fixtures::{fixture, live_facts};
use super::*;
use crate::app::run_with;
use crate::testing::{log, RecordingFacts};
use crate::waits_poll::CheckRunner;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn failed(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

const PR_CHECK: &str = "gh pr view 7 --json state,mergedAt";

fn wait_line(wake: &str) -> String {
    format!("WAIT: build green | check: {PR_CHECK} | wake: {wake}\n")
}

/// A runner that answers every check with `stdout` and counts how often it was asked.
struct Fixed(String, Rc<RefCell<u32>>);
impl CheckRunner for Fixed {
    fn run(&self, _: &Path, _: &[String]) -> Result<String, String> {
        *self.1.borrow_mut() += 1;
        Ok(self.0.clone())
    }
}

type Sent = Rc<RefCell<Vec<(String, String)>>>;

/// A context over `state` whose facts say `alive`, whose checks answer MERGED or OPEN, and whose
/// sender keeps what it was given.
fn fixture_ctx(
    state: &Path,
    lane: &Path,
    alive: bool,
    merged: bool,
) -> (Ctx, Sent, Rc<RefCell<u32>>) {
    let sent: Sent = Rc::default();
    let asked = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&sent);
    let reply = if merged {
        r#"{"state":"MERGED","mergedAt":"x"}"#
    } else {
        r#"{"state":"OPEN","mergedAt":null}"#
    };
    let facts = if alive {
        live_facts()
    } else {
        RecordingFacts::new(&log())
    };
    let ctx = Ctx::capture(state.to_path_buf(), Ok(lane.to_path_buf()), None)
        .with_facts(facts)
        .with_runner(Fixed(reply.into(), Rc::clone(&asked)))
        .with_sender(move |role, text| {
            sink.borrow_mut().push((role.to_string(), text.to_string()));
            Ok(())
        });
    (ctx, sent, asked)
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

#[test]
fn a_passing_wait_is_sent_once_to_its_lane_and_recorded() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, asked) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run(&mut ctx, false)));
    let (out, err) = ctx.captured();
    assert_eq!(err, "");
    assert!(
        out.starts_with("waits-poll: 1 live role(s) read, 1 wait(s) due\n"),
        "{out}"
    );
    assert!(
        out.contains("alpha\tcheck-passed\tbuild green\tSent\n"),
        "{out}"
    );
    assert_eq!(*asked.borrow(), 1);
    let sent = sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "alpha");
    assert!(
        sent[0].1.starts_with("[NOTICE] wait-due affects=alpha "),
        "{}",
        sent[0].1
    );
    assert!(state.join("waits-ledger.json").exists());
}

#[test]
fn a_second_pass_says_already_and_sends_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut first, sent, _) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run(&mut first, false)));
    assert_eq!(sent.borrow().len(), 1);
    let (mut second, sent2, _) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run(&mut second, false)));
    let out = second.captured().0;
    assert!(
        out.contains("alpha\tcheck-passed\tbuild green\tAlready\n"),
        "{out}"
    );
    assert!(sent2.borrow().is_empty());
}

#[test]
fn a_dry_run_reports_but_sends_and_records_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, _) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run(&mut ctx, true)));
    let out = ctx.captured().0;
    assert!(out.contains("(dry run: nothing sent)"), "{out}");
    assert!(
        out.contains("alpha\tcheck-passed\tbuild green\tDry\n"),
        "{out}"
    );
    assert!(sent.borrow().is_empty());
    assert!(!state.join("waits-ledger.json").exists());
}

#[test]
fn a_wait_whose_check_is_false_and_whose_wake_is_ahead_is_not_due() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, asked) = fixture_ctx(&state, &lane, true, false);
    assert!(ok(run(&mut ctx, false)));
    assert_eq!(
        ctx.captured().0,
        "waits-poll: 1 live role(s) read, 0 wait(s) due\n"
    );
    assert_eq!(*asked.borrow(), 1, "the check was run");
    assert!(sent.borrow().is_empty());
}

#[test]
fn the_pm_is_never_polled() {
    // The PM's own HANDOFF is not the supervisor's to read, however live and however due.
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "pm", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, asked) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run(&mut ctx, false)));
    assert_eq!(
        ctx.captured().0,
        "waits-poll: 0 live role(s) read, 0 wait(s) due\n"
    );
    assert_eq!((*asked.borrow(), sent.borrow().len()), (0, 0));
}

#[test]
fn a_lane_whose_process_is_not_alive_is_not_read() {
    // The control for the two tests above: the same fixture, facts that say the pid is gone.
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, asked) = fixture_ctx(&state, &lane, false, true);
    assert!(ok(run(&mut ctx, false)));
    assert_eq!(
        ctx.captured().0,
        "waits-poll: 0 live role(s) read, 0 wait(s) due\n"
    );
    assert_eq!((*asked.borrow(), sent.borrow().len()), (0, 0));
}

#[test]
fn a_failed_send_is_reported_failed_and_not_recorded_so_the_next_pass_retries() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let mut ctx = Ctx::capture(state.clone(), Ok(lane.clone()), None)
        .with_facts(live_facts())
        .with_runner(Fixed(r#"{"state":"MERGED"}"#.into(), Rc::default()))
        .with_sender(|_, _| Err("no peer".into()));
    assert!(ok(run(&mut ctx, false)));
    let out = ctx.captured().0;
    assert!(out.contains("Failed(\"no peer\")"), "{out}");
    assert!(!state.join("waits-ledger.json").exists());
}

#[test]
fn the_command_line_reaches_it_through_run_with_and_a_bad_flag_does_not() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", &wait_line("2099-01-01T00:00:00Z"));
    let (mut ctx, sent, _) = fixture_ctx(&state, &lane, true, true);
    assert!(ok(run_with(&mut ctx, args(&["waits-poll", "--dry-run"]))));
    assert!(ctx.captured().0.contains("(dry run: nothing sent)"));
    assert!(sent.borrow().is_empty());
    // The accepted call above is the control for this refusal.
    let (mut bad, _, _) = fixture_ctx(&state, &lane, true, true);
    assert!(failed(run_with(&mut bad, args(&["waits-poll", "--bogus"]))));
    let (out, err) = bad.captured();
    assert_eq!(out, "");
    assert!(err.starts_with("lane-restart waits-poll: "), "{err}");
}
