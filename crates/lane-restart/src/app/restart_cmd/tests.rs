// SPDX-License-Identifier: MIT OR Apache-2.0

//! The restart command through the `Ctx` seam: a live lane in a tempdir state directory, fake process
//! facts that record every kill (so no process is ever killed), a fake verified caller, a launch
//! spawner that opens no window, no real sleep, no `LANE_RESTART_ALLOW_OPUS`, and the `restart.log`
//! rows read back. The machine's state directory and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::app::test_fixtures::{
    age_lane_state, coming_up_spawn, failing_spawn, fixture, live_facts, settle_handoff,
    LiveLaneFacts, UnderClaude,
};
use crate::facts::ProcEntry;
use crate::start::CallerError;
use crate::testing::{log, Log, RecordingFacts};
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn code_is(code: ExitCode, want: u8) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::from(want))
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// A context over `state` whose verified caller is `caller` (`None`: it cannot be verified), with no
/// environment (so no Opus permit) and a launch spawner that fails every time unless replaced.
fn ctx(
    state: &Path,
    lane: &Path,
    caller: Option<&'static str>,
    facts: RecordingFacts,
    lane_role: Option<&str>,
    calls: &Rc<RefCell<u32>>,
) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    Ctx::capture(state.to_path_buf(), Ok(lane.to_path_buf()), lane_role)
        .with_facts(LiveLaneFacts {
            inner: facts,
            lane: lane.to_path_buf(),
        })
        .with_parents(UnderClaude)
        .with_caller(make)
        .with_env(&[])
        .with_launch_spawn(failing_spawn(calls))
        .with_sleeper(Rc::new(|_| {}))
}

/// A live lane `alpha` ready to be restarted, and the facts that record into `l`.
fn lane(root: &Path) -> (PathBuf, PathBuf, Log, RecordingFacts) {
    let (state, lane) = fixture(root, "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    (state, lane, l, facts)
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .map(|l| l.rows)
        .unwrap_or_default()
}

fn kills(l: &Log) -> Vec<String> {
    l.borrow()
        .iter()
        .filter(|e| e.starts_with("kill "))
        .cloned()
        .collect()
}

fn proc(pid: u32, parent: Option<u32>, name: &str, start: u64) -> ProcEntry {
    ProcEntry {
        pid,
        parent,
        name: name.to_string(),
        start_time_secs: start,
        exe: Some(PathBuf::from(format!(r"C:\bin\{name}"))),
        cmd: Vec::new(),
    }
}

#[test]
fn a_lane_that_is_not_the_pm_cannot_restart_another_and_nothing_is_killed() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, Some("beta"), facts, None, &calls);
    let code = run_with(&mut c, args(&["--role", "alpha", "--yes"]));
    assert!(code_is(code, 1), "{code:?}");
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(err.contains("lane-restart: refused - "), "{err}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].kind.as_deref(), Some("restart"));
    assert_eq!(logged[0].requested_by, "beta");
    assert_eq!(logged[0].caller_role.as_deref(), Some("beta"));
    assert!(
        logged[0].outcome.starts_with("refused: "),
        "{:?}",
        logged[0]
    );
    assert!(!logged[0].acted);
    assert!(kills(&l).is_empty());
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, None, facts, None, &calls);
    let code = run_with(&mut c, args(&["--role", "alpha", "--yes"]));
    assert!(code_is(code, 1), "{code:?}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].requested_by, "unverified");
    assert!(logged[0].caller_role.is_none());
    assert!(kills(&l).is_empty());
}

#[test]
fn a_pm_restart_without_yes_is_a_dry_run_that_kills_and_launches_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, Some("pm"), facts, None, &calls);
    let code = run_with(&mut c, args(&["--role", "alpha"]));
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert!(
        out.starts_with(
            "lane-restart: DRY RUN - would kill pid 55 and relaunch a FRESH session via wt.exe"
        ),
        "{out}"
    );
    assert!(
        out.contains("lane-restart: DRY RUN - would leave the tab's shell alone: "),
        "{out}"
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].outcome, "dry run - no action taken");
    assert_eq!(logged[0].requested_by, "pm");
    assert!(!logged[0].acted);
    assert!(kills(&l).is_empty(), "a dry run killed something");
    assert_eq!(*calls.borrow(), 0);
}

#[test]
fn a_pm_restart_with_yes_kills_relaunches_and_closes_the_leftover_tab_last() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, mut facts) = lane(d.path());
    facts.table = Ok(vec![
        proc(2812, Some(1), "WindowsTerminal.exe", 10),
        proc(16500, Some(2812), "pwsh.exe", 50),
        proc(55, Some(16500), "claude.exe", 100),
    ]);
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, Some("pm"), facts, None, &calls)
        .with_launch_spawn(coming_up_spawn(&state, &calls));
    let code = run_with(&mut c, args(&["--role", "alpha", "--yes"]));
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert!(
        err.contains("lane-restart: killing pid 55 and relaunching a fresh session..."),
        "{err}"
    );
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(
        kills(&l),
        vec![
            "kill 55 start=100".to_string(),
            "kill 16500 start=50".to_string()
        ],
        "claude is killed first and the leftover shell after"
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(
        logged[0].outcome,
        "killed pid 55 and relaunched a fresh session (continuity via HANDOFF only)"
    );
    assert!(logged[0].acted);
    assert_eq!(logged[0].kind.as_deref(), Some("restart"));
    assert!(
        logged[1]
            .outcome
            .starts_with("closing the leftover tab: terminating shell "),
        "{:?}",
        logged[1]
    );
    let spec = crate::spec::read(&state, "alpha").unwrap();
    assert_eq!(spec.state, crate::spec::SpecState::Running);
}

#[test]
fn a_session_stuck_at_the_dev_channels_dialog_is_reported_awaiting_with_exit_2() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, _l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    let spawned = Rc::clone(&calls);
    // The spawn succeeds, nothing ever writes progress, and a process is seen: after the progress
    // timeout the launch is reported as waiting for a person. The sleep is virtual.
    let mut c = ctx(&state, &lane, Some("pm"), facts, None, &calls).with_launch_spawn(Rc::new(
        move |_, _| {
            *spawned.borrow_mut() += 1;
            Ok(())
        },
    ));
    let code = run_with(&mut c, args(&["--role", "alpha", "--yes"]));
    let (out, err) = c.captured();
    assert!(code_is(code, 2), "{code:?}\n{out}\n{err}");
    let line: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(line["event"], "lane-restart-status");
    assert_eq!(line["outcome"], "awaiting_confirmation");
    assert_eq!(line["role"], "alpha");
    let logged = rows(&state);
    assert_eq!(
        logged[0].outcome,
        "awaiting human confirmation at the dev-channels dialog"
    );
    assert!(logged[0].acted);
}

#[test]
fn a_relaunch_that_fails_is_recorded_failed_and_the_tab_is_left_alone() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, mut facts) = lane(d.path());
    facts.table = Ok(vec![
        proc(2812, Some(1), "WindowsTerminal.exe", 10),
        proc(16500, Some(2812), "pwsh.exe", 50),
        proc(55, Some(16500), "claude.exe", 100),
    ]);
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, Some("pm"), facts, None, &calls);
    let code = run_with(&mut c, args(&["--role", "alpha", "--yes"]));
    let (_, err) = c.captured();
    assert!(code_is(code, 1), "{code:?}\n{err}");
    assert!(err.contains("no window in a test"), "{err}");
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(
        kills(&l),
        vec!["kill 55 start=100".to_string()],
        "after a failed relaunch the shell must be left so a person has a prompt"
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert!(
        logged[0].outcome.starts_with("action failed: "),
        "{:?}",
        logged[0]
    );
    assert!(!logged[0].acted);
    assert!(
        logged[1]
            .outcome
            .starts_with("leftover tab's shell left alone: "),
        "{:?}",
        logged[1]
    );
}

#[test]
fn a_bare_self_restarts_the_callers_own_lane_found_by_lane_role_and_is_recorded_as_self() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    let mut c = ctx(&state, &lane, Some("alpha"), facts, Some("alpha"), &calls)
        .with_launch_spawn(coming_up_spawn(&state, &calls));
    let code = run_with(&mut c, args(&["--self"]));
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert_eq!(kills(&l), vec!["kill 55 start=100".to_string()]);
    let logged = rows(&state);
    assert_eq!(logged[0].requested_by, "self");
    assert_eq!(logged[0].caller_role.as_deref(), None);
    assert_eq!(logged[0].role, "alpha");
}

#[test]
fn a_bare_self_that_finds_no_role_says_so_and_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane, l, facts) = lane(d.path());
    let calls = Rc::new(RefCell::new(0));
    // No LANE_ROLE, and a working directory that is no lane's: nothing to resolve.
    let elsewhere = d.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut c = ctx(&state, &elsewhere, Some("alpha"), facts, None, &calls);
    let code = run_with(&mut c, args(&["--self"]));
    let (out, err) = c.captured();
    assert!(code_is(code, 1), "{code:?}");
    assert_eq!(out, "");
    assert!(
        err.starts_with("lane-restart: --self without --role: "),
        "{err}"
    );
    assert!(rows(&state).is_empty());
    assert!(kills(&l).is_empty());
    let _ = lane;
}
