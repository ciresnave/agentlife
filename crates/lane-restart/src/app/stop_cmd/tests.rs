// SPDX-License-Identifier: MIT OR Apache-2.0

//! `stop` through the `Ctx` seam: a live lane in a tempdir state directory, fake process facts that
//! record every kill (so no process is ever killed), a fake verified caller, no real sleep, and the
//! `restart.log` rows read back. The machine's state directory and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::app::test_fixtures::{
    age_lane_state, fixture, live_facts, settle_handoff, LiveLaneFacts, UnderClaude,
};
use crate::facts::ProcEntry;
use crate::start::CallerError;
use crate::testing::{log, RecordingFacts};
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

/// A context over `state` whose verified caller is `caller` (`None`: it cannot be verified).
fn ctx(state: &Path, lane: &Path, caller: Option<&'static str>, facts: RecordingFacts) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None)
        .with_facts(LiveLaneFacts {
            inner: facts,
            lane: lane.to_path_buf(),
        })
        .with_parents(UnderClaude)
        .with_caller(make)
        .with_sleeper(Rc::new(|_| {}))
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .map(|l| l.rows)
        .unwrap_or_default()
}

fn spec_bytes(state: &Path) -> Vec<u8> {
    std::fs::read(state.join("alpha.launch.json")).unwrap()
}

fn kills(l: &crate::testing::Log) -> Vec<String> {
    l.borrow()
        .iter()
        .filter(|e| e.starts_with("kill "))
        .cloned()
        .collect()
}

#[test]
fn a_lane_that_is_not_the_pm_cannot_stop_another_and_nothing_changes() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let before = spec_bytes(&state);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    let mut c = ctx(&state, &lane, Some("beta"), facts);
    let code = run_with(
        &mut c,
        args(&[
            "stop",
            "--role",
            "alpha",
            "--reason",
            "a test stop",
            "--yes",
        ]),
    );
    assert!(code_is(code, 1), "{code:?}");
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(err.contains("lane-restart stop: refused - "), "{err}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].kind.as_deref(), Some("stop"));
    assert_eq!(logged[0].requested_by, "beta");
    assert_eq!(logged[0].caller_role.as_deref(), Some("beta"));
    assert!(
        logged[0].outcome.starts_with("refused: "),
        "{:?}",
        logged[0]
    );
    assert!(!logged[0].acted);
    assert!(kills(&l).is_empty(), "a refused stop killed something");
    assert_eq!(spec_bytes(&state), before);
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    let mut c = ctx(&state, &lane, None, facts);
    let code = run_with(
        &mut c,
        args(&[
            "stop",
            "--role",
            "alpha",
            "--reason",
            "a test stop",
            "--yes",
        ]),
    );
    assert!(code_is(code, 1), "{code:?}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].requested_by, "unverified");
    assert!(logged[0].caller_role.is_none());
    assert!(kills(&l).is_empty());
}

#[test]
fn a_pm_stop_without_yes_is_a_dry_run_that_kills_and_writes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let before = spec_bytes(&state);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    let mut c = ctx(&state, &lane, Some("pm"), facts);
    let code = run_with(
        &mut c,
        args(&["stop", "--role", "alpha", "--reason", "a test stop"]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert!(
        out.starts_with("lane-restart stop: DRY RUN - would write the launch spec for alpha"),
        "{out}"
    );
    assert!(
        out.contains("then kill pid 55 and mark the spec suspended"),
        "{out}"
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(
        logged[0].outcome.starts_with("dry run - no action taken"),
        "{:?}",
        logged[0]
    );
    assert!(!logged[0].acted);
    assert_eq!(logged[0].reason.as_deref(), Some("a test stop"));
    assert!(kills(&l).is_empty(), "a dry run killed something");
    assert_eq!(spec_bytes(&state), before);
}

#[test]
fn a_pm_stop_with_yes_kills_the_verified_pid_and_suspends_the_spec() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    let mut c = ctx(&state, &lane, Some("pm"), facts);
    let code = run_with(
        &mut c,
        args(&[
            "stop",
            "--role",
            "alpha",
            "--reason",
            "a test stop",
            "--yes",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert!(
        err.contains("lane-restart stop: writing the launch spec, then stopping alpha (pid 55)..."),
        "{err}"
    );
    assert_eq!(kills(&l), vec!["kill 55 start=100".to_string()]);
    let spec = crate::spec::read(&state, "alpha").unwrap();
    assert_eq!(spec.state, crate::spec::SpecState::Suspended);
    assert_eq!(spec.stop_kind, Some(crate::spec::StopKind::Hard));
    let logged = rows(&state);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(
        logged[0].outcome,
        "stopped pid 55 (hard stop); launch spec suspended"
    );
    assert!(logged[0].acted);
    assert_eq!(logged[0].requested_by, "pm");
    assert_eq!(logged[0].model.as_deref(), Some("sonnet"));
    // No process table in the fake: the shell the tab would close is left alone, and says so.
    assert!(
        logged[1]
            .outcome
            .starts_with("leftover tab's shell left alone: "),
        "{:?}",
        logged[1]
    );
    assert!(!logged[1].acted);
}

#[test]
fn a_lane_can_stop_itself_softly_and_is_recorded_as_self() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    let mut c = ctx(&state, &lane, Some("alpha"), facts);
    let code = run_with(
        &mut c,
        args(&[
            "stop",
            "--role",
            "alpha",
            "--reason",
            "wrapping up",
            "--self",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert_eq!(kills(&l), vec!["kill 55 start=100".to_string()]);
    let spec = crate::spec::read(&state, "alpha").unwrap();
    assert_eq!(spec.stop_kind, Some(crate::spec::StopKind::Soft));
    let logged = rows(&state);
    assert_eq!(logged[0].requested_by, "self");
    assert_eq!(logged[0].caller_role.as_deref(), Some("alpha"));
    assert_eq!(
        logged[0].outcome,
        "stopped pid 55 (soft stop); launch spec suspended"
    );
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

/// The hand-started shape (a terminal, a shell, the lane's claude): after the stop the shell's tab
/// is closed through the context's facts, the closing is logged first, and the kill is recorded.
#[test]
fn a_hand_started_lanes_leftover_shell_is_closed_and_logged_before_the_kill() {
    let d = tempfile::tempdir().unwrap();
    let (state, lane) = fixture(d.path(), "alpha", "# h\n");
    age_lane_state(&state, "alpha", 5);
    settle_handoff(&lane);
    let l = log();
    let mut facts = live_facts();
    facts.log = Rc::clone(&l);
    facts.table = Ok(vec![
        proc(2812, Some(1), "WindowsTerminal.exe", 10),
        proc(16500, Some(2812), "pwsh.exe", 50),
        proc(55, Some(16500), "claude.exe", 100),
    ]);
    let mut c = ctx(&state, &lane, Some("pm"), facts);
    let code = run_with(
        &mut c,
        args(&[
            "stop",
            "--role",
            "alpha",
            "--reason",
            "a test stop",
            "--yes",
        ]),
    );
    let (out, err) = c.captured();
    assert!(code_is(code, 0), "{code:?}\n{out}\n{err}");
    assert_eq!(
        kills(&l),
        vec![
            "kill 55 start=100".to_string(),
            "kill 16500 start=50".to_string()
        ]
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert!(
        logged[1]
            .outcome
            .starts_with("closing the leftover tab: terminating shell "),
        "{:?}",
        logged[1]
    );
    assert!(logged[1].acted);
    assert_eq!(logged[1].kind.as_deref(), Some("stop"));
}
