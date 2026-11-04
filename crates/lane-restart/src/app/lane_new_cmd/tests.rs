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

/// `<base>/proj/<repo>` as a checkout and `<base>/proj/<repo>-wt-<topic>` as a worktree of it (its `.git`
/// is a file pointing into the checkout's `.git/worktrees`, as `git worktree add` writes it).
fn worktree(base: &Path, repo: &str, topic: &str) -> PathBuf {
    let primary = base.join("proj").join(repo);
    std::fs::create_dir_all(primary.join(".git").join("worktrees").join(topic)).unwrap();
    let wt = base.join("proj").join(format!("{repo}-wt-{topic}"));
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!(
            "gitdir: {}\n",
            primary.join(".git").join("worktrees").join(topic).display()
        ),
    )
    .unwrap();
    wt
}

fn lnw(role: &str, wt: &Path, rest: &[&str]) -> Vec<OsString> {
    let mut v = vec!["lane-new-worktree", role, "zetafish"];
    let w = wt.to_string_lossy().into_owned();
    let mut a = args(&v);
    a.push(OsString::from(w));
    v.clear();
    a.extend(rest.iter().map(OsString::from));
    a
}

#[test]
fn a_worktree_lane_dry_runs_then_applies_with_the_worktree_as_its_cwd_and_no_repo_steps() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let wt = worktree(d.path(), "zetafish", "pkg-b");
    let before = listing(&state);
    let mut dry = ctx(Ok(r.clone()), &state, Some("pm"));
    let code = run_with(&mut dry, lnw("zetafish-b", &wt, &[]));
    let (out, err) = dry.captured();
    assert!(ok(code), "{out}\n{err}");
    assert_eq!(listing(&state), before, "a dry run changed the state dir");
    for n in ["create repo", "clone + seed", "protect main"] {
        assert!(!out.contains(n), "a worktree lane has no step {n:?}: {out}");
    }
    let plan = plan_of(&out);
    let mut c = ctx(Ok(r), &state, Some("pm"));
    let code = run_with(
        &mut c,
        lnw("zetafish-b", &wt, &["--apply", "--plan", &plan]),
    );
    let (out, err) = c.captured();
    assert!(ok(code), "{out}\n{err}");
    let spec = crate::spec::read(&state, "zetafish-b").unwrap();
    assert_eq!(spec.cwd, wt.to_string_lossy());
    assert_eq!(
        spec.handoff_path.as_deref(),
        Some(&*wt.join("HANDOFF.md").to_string_lossy())
    );
    assert_eq!(spec.state, crate::spec::SpecState::Suspended);
    assert!(spec.stop_kind.is_none());
    let logged = rows(&state);
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(logged[0].acted);
    assert!(
        logged[0].reason.as_deref().unwrap().contains("worktree"),
        "{:?}",
        logged[0]
    );
}

#[test]
fn a_plan_is_bound_to_its_worktree_and_a_non_pm_caller_is_refused_by_command_name() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let b = worktree(d.path(), "zetafish", "pkg-b");
    let c_wt = worktree(d.path(), "zetafish", "pkg-c");
    let mut dry = ctx(Ok(r.clone()), &state, Some("pm"));
    assert!(ok(run_with(&mut dry, lnw("zetafish-b", &b, &[]))));
    let plan = plan_of(&dry.captured().0);
    let before = listing(&state);
    let mut c = ctx(Ok(r.clone()), &state, Some("pm"));
    let code = run_with(
        &mut c,
        lnw("zetafish-b", &c_wt, &["--apply", "--plan", &plan]),
    );
    assert!(failed(code));
    assert!(c.captured().0.contains("is not the plan for role"));
    let mut after = listing(&state);
    after.retain(|n| n != "restart.log");
    assert_eq!(after, before);
    let mut other = ctx(Ok(r), &state, Some("alpha"));
    assert!(failed(run_with(&mut other, lnw("zetafish-b", &b, &[]))));
    assert!(
        other
            .captured()
            .1
            .contains("lane-restart lane-new-worktree: refused - only the PM session"),
        "refusal must name the command that was refused"
    );
}

#[test]
fn a_path_that_is_not_a_worktree_of_the_repo_is_refused_and_the_real_one_is_not() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let good = worktree(d.path(), "zetafish", "pkg-b");
    let plain = d.path().join("proj").join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let elsewhere = d.path().join("proj").join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(
        elsewhere.join(".git"),
        "gitdir: /somewhere/else/.git/worktrees/x\n",
    )
    .unwrap();
    let primary = d.path().join("proj").join("zetafish");
    for (bad, why) in [
        (plain, "no .git file"),
        (elsewhere, "does not point into"),
        (primary, "the primary checkout itself"),
        (PathBuf::from("rel/path"), "not absolute"),
        (d.path().join("proj").join("missing"), "no .git file"),
    ] {
        let mut c = ctx(Ok(r.clone()), &state, Some("pm"));
        let code = run_with(&mut c, lnw("zetafish-b", &bad, &[]));
        let (out, _) = c.captured();
        assert!(failed(code), "{bad:?} accepted: {out}");
        assert!(out.contains(why), "{bad:?}: wanted {why:?} in {out}");
    }
    let mut c = ctx(Ok(r), &state, Some("pm"));
    let code = run_with(&mut c, lnw("zetafish-b", &good, &[]));
    assert!(
        ok(code),
        "control: a real worktree is accepted: {}",
        c.captured().0
    );
}

#[test]
fn a_worktree_of_a_repo_with_no_checkout_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    let state = r.state_dir.clone();
    let wt = d.path().join("proj").join("zetafish-wt-x");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: x\n").unwrap();
    let mut c = ctx(Ok(r), &state, Some("pm"));
    assert!(failed(run_with(&mut c, lnw("zetafish-b", &wt, &[]))));
    assert!(c.captured().0.contains("no repo to take a worktree of"));
}
