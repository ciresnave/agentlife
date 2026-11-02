// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-new` through the `Ctx` seam: roots in a tempdir, a fake verified caller, the local-file
//! steps only (no GitHub client, no host), and the `restart.log` rows read back. The machine's
//! state directory, GitHub and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::lane_new::Roots;
use crate::start::CallerError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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

fn roots(base: &Path) -> Roots {
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::write(
        base.join("state").join("roles-soak.txt"),
        "# soak\npm-assist\nfuel\n",
    )
    .unwrap();
    Roots::new(
        base.join("state"),
        base.join("proj"),
        base.join("homes"),
        base.join("pilot"),
        base.join("bin").join("chan"),
    )
    .unwrap()
}

/// A context over `roots.state_dir` whose verified caller is `caller` (`None`: cannot be verified).
fn ctx(roots: Result<Roots, String>, state: &Path, caller: Option<&'static str>) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None)
        .with_lane_new(roots, None, None)
        .with_caller(make)
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .map(|l| l.rows)
        .unwrap_or_default()
}

fn listing(state: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(state)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn plan_of(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("plan "))
        .expect("a dry run ends with a plan hash")
        .to_string()
}

#[test]
fn a_non_pm_caller_is_refused_by_name_and_recorded_and_nothing_is_created() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let before = listing(&state);
    let mut c = ctx(Ok(r), &state, Some("alpha"));
    let code = run_with(&mut c, args(&["lane-new", "zetafish", "zetafish"]));
    assert!(failed(code));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(
        err.contains(
            "refused - only the PM session may create a lane; this is running under \"alpha\""
        ),
        "{err}"
    );
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].kind.as_deref(), Some("lane-new"));
    assert_eq!(logged[0].role, "zetafish");
    assert_eq!(logged[0].requested_by, "alpha");
    assert!(!logged[0].acted);
    assert!(
        logged[0].outcome.starts_with("refused: "),
        "{:?}",
        logged[0]
    );
    let mut after = listing(&state);
    after.retain(|n| n != "restart.log");
    assert_eq!(after, before);
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let mut c = ctx(Ok(r), &state, None);
    let code = run_with(&mut c, args(&["lane-new", "zetafish", "zetafish"]));
    assert!(failed(code));
    let (_, err) = c.captured();
    assert!(err.contains("the caller could not be verified"), "{err}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].requested_by, "unverified");
    assert!(!logged[0].acted);
}

#[test]
fn roots_that_cannot_be_built_stop_the_pm_before_anything_is_logged_or_created() {
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut c = ctx(Err("LOCALAPPDATA is not set".into()), &state, Some("pm"));
    let code = run_with(&mut c, args(&["lane-new", "zetafish", "zetafish"]));
    assert!(failed(code));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert_eq!(err, "lane-restart lane-new: LOCALAPPDATA is not set\n");
    assert!(rows(&state).is_empty());
    assert!(listing(&state).is_empty());
}

#[test]
fn a_pm_dry_run_prints_the_plan_and_writes_and_logs_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let before = listing(&state);
    let mut c = ctx(Ok(r), &state, Some("pm"));
    let code = run_with(&mut c, args(&["lane-new", "zetafish", "zetafish"]));
    let (out, err) = c.captured();
    assert!(ok(code), "{out}\n{err}");
    assert_eq!(err, "");
    assert!(
        out.contains("to act, repeat the same role, repo and model with: --apply --plan "),
        "{out}"
    );
    assert_eq!(listing(&state), before, "a dry run changed the state dir");
    assert!(rows(&state).is_empty(), "a dry run is not recorded");
}

#[test]
fn apply_without_a_plan_is_refused_and_recorded_as_failed_and_not_acted() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let before = listing(&state);
    let mut c = ctx(Ok(r), &state, Some("pm"));
    let code = run_with(
        &mut c,
        args(&["lane-new", "zetafish", "zetafish", "--apply"]),
    );
    assert!(failed(code));
    let (out, _) = c.captured();
    assert!(out.contains("--apply needs --plan"), "{out}");
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(
        logged[0].outcome.starts_with("failed: 0:Fail"),
        "{:?}",
        logged[0]
    );
    assert!(!logged[0].acted);
    assert_eq!(logged[0].model.as_deref(), Some("sonnet"));
    assert_eq!(logged[0].reason.as_deref(), Some("repo zetafish"));
    let mut after = listing(&state);
    after.retain(|n| n != "restart.log");
    assert_eq!(after, before);
}

#[test]
fn apply_with_the_dry_runs_plan_writes_the_lane_files_and_records_what_was_done() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state: PathBuf = r.state_dir.clone();
    let mut dry = ctx(Ok(r.clone()), &state, Some("pm"));
    assert!(ok(run_with(
        &mut dry,
        args(&["lane-new", "zetafish", "zetafish"])
    )));
    let plan = plan_of(&dry.captured().0);
    let before = listing(&state);
    let mut c = ctx(Ok(r), &state, Some("pm"));
    let code = run_with(
        &mut c,
        args(&[
            "lane-new", "zetafish", "zetafish", "--apply", "--plan", &plan,
        ]),
    );
    let (out, err) = c.captured();
    assert!(ok(code), "{out}\n{err}");
    assert_ne!(listing(&state), before, "apply changed nothing");
    assert!(state.join("zetafish.launch.json").exists());
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(
        logged[0].outcome.starts_with("created: "),
        "{:?}",
        logged[0]
    );
    assert!(logged[0].acted);
    assert_eq!(logged[0].kind.as_deref(), Some("lane-new"));
    assert_eq!(logged[0].caller_role.as_deref(), Some("pm"));
}
