// SPDX-License-Identifier: MIT OR Apache-2.0

//! `status` through the `Ctx` seam: a fixture state directory with a LIVE lane (fake process
//! facts), a fake notify transport, a fake broker and a fake process-parent walk, and the text the
//! command printed.

use super::*;
use crate::app::run_with;
use crate::app::test_fixtures::{age_lane_state, fixture, live_facts};
use crate::notify::{Peer, Transport};
use crate::testing::{log, RecordingFacts};
use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn failed(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// Maps the broker helper pid 11 to the live claude (pid 55), as a real chain would.
struct Parents;
impl crate::lane_state_writer::ParentProcess for Parents {
    fn parent_of(&self, pid: u32) -> Option<(u32, String)> {
        (pid == 11).then(|| (55, "claude.exe".to_string()))
    }
    fn cmdline_of(&self, _: u32) -> Option<Vec<String>> {
        None
    }
}

fn peer(id: &str, pid: u32) -> Peer {
    Peer {
        id: id.into(),
        cwd: "C:/x".into(),
        pid,
        summary: Some("hi".into()),
        last_seen: None,
    }
}

fn base(state: &Path, lane: &Path) -> Ctx {
    Ctx::capture(state.to_path_buf(), Ok(lane.to_path_buf()), None)
        .with_facts(live_facts())
        .with_parents(Parents)
        .with_transport(Transport::Broker)
        .with_broker_peers(|| panic!("the broker must not be asked unless --peers is given"))
}

fn live_lane() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# handoff\n");
    (d, state, lane)
}

#[test]
fn the_text_view_lists_the_live_role_with_its_handoff_and_the_spec_coverage() {
    let (_d, state, lane) = live_lane();
    let mut ctx = base(&state, &lane);
    assert!(ok(run(&mut ctx, false, None, false)));
    let (out, err) = ctx.captured();
    assert_eq!(err, "");
    let lines: Vec<&str> = out.lines().collect();
    // First the disk line, then one row per role, then the coverage line.
    assert!(
        lines[0].contains("free:") || lines[0].starts_with("disk free: unknown"),
        "{out}"
    );
    let row = lines.iter().find(|l| l.starts_with("alpha\t")).expect(&out);
    assert!(
        row.starts_with("alpha\tRunning\tspec=Complete\tstartable=false\tidle="),
        "{row}"
    );
    assert!(row.contains("\thandoff=10B/8192B"), "{row}");
    assert!(
        out.contains("launch specs: 1 of 1 role(s) have a complete one\n"),
        "{out}"
    );
}

#[test]
fn an_empty_state_directory_has_no_roles_and_says_so_in_the_coverage_line() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut ctx = base(&state, d.path());
    assert!(ok(run(&mut ctx, false, None, false)));
    let out = ctx.captured().0;
    assert!(
        out.contains("launch specs: 0 of 0 role(s) have a complete one\n"),
        "{out}"
    );
    assert_eq!(out.lines().count(), 2, "{out}");
}

#[test]
fn json_puts_the_report_on_stdout_and_the_disk_line_on_stderr() {
    let (_d, state, lane) = live_lane();
    let mut ctx = base(&state, &lane);
    assert!(ok(run(&mut ctx, true, None, false)));
    let (out, err) = ctx.captured();
    let v: serde_json::Value = serde_json::from_str(&out).expect(&out);
    assert_eq!(v["roles"][0]["role"], "alpha");
    assert_eq!(v["roles"][0]["state"], "running");
    assert!(err.ends_with('\n') && err.lines().count() == 1, "{err}");
}

#[test]
fn quiet_over_marks_a_role_quiet_longer_than_the_threshold_and_not_a_fresh_one() {
    let (_d, state, lane) = live_lane();
    age_lane_state(&state, "alpha", 3600);
    let mut ctx = base(&state, &lane);
    assert!(ok(run(&mut ctx, false, Some(120), false)));
    assert!(
        ctx.captured().0.contains(" QUIET>120s"),
        "{}",
        ctx.captured().0
    );
    // The control: the same lane with a threshold it has not passed, and with no threshold.
    let mut ctx = base(&state, &lane);
    assert!(ok(run(&mut ctx, false, Some(86_400), false)));
    assert!(!ctx.captured().0.contains("QUIET>"));
    let mut ctx = base(&state, &lane);
    assert!(ok(run(&mut ctx, false, None, false)));
    assert!(!ctx.captured().0.contains("QUIET>"));
}

#[test]
fn peers_with_the_broker_transport_join_a_session_to_the_role_by_its_parent() {
    let (_d, state, lane) = live_lane();
    let mut ctx =
        base(&state, &lane).with_broker_peers(|| Ok(vec![peer("p1", 11), peer("p2", 22)]));
    assert!(ok(run(&mut ctx, false, None, true)));
    let out = ctx.captured().0;
    assert!(out.contains("  peers: p1 \"hi\"\n"), "{out}");
    assert!(
        out.contains("unmatched peer p2 in C:/x: no role's live claude is its parent - \"hi\"\n"),
        "{out}"
    );
}

#[test]
fn an_unreadable_broker_makes_every_role_unknown_never_no_peer() {
    let (_d, state, lane) = live_lane();
    let mut ctx = base(&state, &lane).with_broker_peers(|| Err("down".into()));
    assert!(ok(run(&mut ctx, false, None, true)));
    let out = ctx.captured().0;
    assert!(out.contains("  peers: unknown\n"), "{out}");
    assert!(
        out.contains("(peers: the broker could not be read, so every role is unknown: down)\n"),
        "{out}"
    );
}

#[test]
fn peers_with_the_synapse_transport_is_moved_and_the_broker_is_never_asked() {
    let (_d, state, lane) = live_lane();
    let asked = Rc::new(Cell::new(false));
    let seen = Rc::clone(&asked);
    let mut ctx = base(&state, &lane)
        .with_transport(Transport::Synapse)
        .with_broker_peers(move || {
            seen.set(true);
            Ok(vec![])
        });
    assert!(ok(run(&mut ctx, false, None, true)));
    let (out, err) = ctx.captured();
    assert_eq!(
        err,
        "lane-restart status --peers: moved (notify_transport is synapse): run `agentlife status --peers`\n"
    );
    assert!(!out.contains("  peers:"), "{out}");
    assert!(!asked.get());
}

#[test]
fn a_role_whose_process_is_gone_is_reported_not_running() {
    let (_d, state, lane) = live_lane();
    let mut ctx = Ctx::capture(state.clone(), Ok(lane), None)
        .with_facts(RecordingFacts::new(&log()))
        .with_transport(Transport::Broker);
    assert!(ok(run(&mut ctx, false, None, false)));
    let out = ctx.captured().0;
    let row = out.lines().find(|l| l.starts_with("alpha\t")).expect(&out);
    assert!(row.contains("\tCrashed\t"), "{row}");
    assert!(row.contains("idle=-"), "{row}");
}

#[test]
fn the_command_line_reaches_it_through_run_with_and_a_bad_flag_does_not() {
    let (_d, state, lane) = live_lane();
    let mut ctx = base(&state, &lane);
    assert!(ok(run_with(&mut ctx, args(&["status", "--json"]))));
    assert!(ctx.captured().0.starts_with('{'));
    // The accepted call above is the control for the refusals below.
    for bad in [
        &["status", "--bogus"][..],
        &["status", "--quiet-over"][..],
        &["status", "--quiet-over", "x"][..],
    ] {
        let mut ctx = base(&state, &lane);
        assert!(failed(run_with(&mut ctx, args(bad))), "{bad:?}");
        let (out, err) = ctx.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with("lane-restart status: "), "{err}");
        assert!(
            err.contains("expected `status [--json] [--quiet-over <secs>] [--peers]`"),
            "{err}"
        );
    }
}
