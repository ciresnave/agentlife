// SPDX-License-Identifier: MIT OR Apache-2.0

//! `install` through the `Ctx` seam: a hooks directory and a candidate file in a tempdir, a fake
//! probe (nothing is run), a fake verified caller, and the `restart.log` rows read back. The real
//! machine's hooks directory and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::install::{Probe, BAK_NAME, EXE_NAME, SHA_NAME};
use crate::start::CallerError;
use std::ffi::OsString;
use std::path::Path;

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

/// A probe that calls every candidate `lane-restart 9.9.9` and healthy.
struct Healthy;
impl Probe for Healthy {
    fn version_output(&self, _: &Path) -> Result<String, String> {
        Ok("lane-restart 9.9.9\nembedded approvals: none\n".to_string())
    }
    fn status_ok(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
}

const CANDIDATE_BYTES: &[u8] = b"a freshly built lane-restart";

struct Rig {
    _dir: tempfile::TempDir,
    state: std::path::PathBuf,
    hooks: std::path::PathBuf,
    candidate: std::path::PathBuf,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let candidate = dir.path().join("candidate.exe");
    std::fs::write(&candidate, CANDIDATE_BYTES).unwrap();
    Rig {
        _dir: dir,
        state,
        hooks,
        candidate,
    }
}

fn ctx(rig: &Rig, caller: Result<&'static str, CallerError>) -> Ctx {
    let caller = std::cell::RefCell::new(Some(caller));
    // `CallerError` is not `Clone`, so the closure rebuilds the answer it was given each call.
    let text = caller.borrow_mut().take().unwrap();
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match text {
        Ok(role) => Box::new(move || Ok(role.to_string())),
        Err(CallerError::NoClaudeAncestor) => Box::new(|| Err(CallerError::NoClaudeAncestor)),
        Err(CallerError::Unrecognised(p)) => Box::new(move || Err(CallerError::Unrecognised(p))),
        Err(CallerError::Ambiguous(p)) => Box::new(move || Err(CallerError::Ambiguous(p))),
    };
    Ctx::capture(rig.state.clone(), Ok(rig.hooks.clone()), None)
        .with_hooks_dir(rig.hooks.clone())
        .with_probe(Healthy)
        .with_caller(make)
}

fn rows(rig: &Rig) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&rig.state.join("restart.log"))
        .unwrap()
        .rows
}

fn sha_of_candidate() -> String {
    crate::install::sha256_hex(CANDIDATE_BYTES)
}

#[test]
fn a_caller_that_is_not_the_pm_is_refused_by_name_and_the_refusal_is_in_the_ledger() {
    let r = rig();
    let mut c = ctx(&r, Ok("alpha"));
    assert!(failed(run(&mut c, r.candidate.clone(), false)));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert_eq!(
        err,
        "lane-restart install: refused - only the PM session may install the binary; this is running under \"alpha\"\n"
    );
    let rows = rows(&r);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].requested_by.as_str(),
            rows[0].role.as_str(),
            rows[0].kind.as_deref(),
            rows[0].acted
        ),
        ("alpha", EXE_NAME, Some("install"), false)
    );
    assert!(rows[0]
        .outcome
        .starts_with("refused: only the PM session may install"));
    // Nothing was installed.
    assert!(!r.hooks.join(EXE_NAME).exists());
}

#[test]
fn an_unverifiable_caller_is_refused_and_recorded_as_unverified() {
    let r = rig();
    let mut c = ctx(&r, Err(CallerError::NoClaudeAncestor));
    assert!(failed(run(&mut c, r.candidate.clone(), false)));
    let err = c.captured().1;
    assert!(
        err.contains("and the caller could not be verified: this command is not running under a claude session"),
        "{err}"
    );
    let rows = rows(&r);
    assert_eq!(rows[0].requested_by, "unverified");
    assert!(!rows[0].acted);
    assert!(!r.hooks.join(EXE_NAME).exists());
}

#[test]
fn the_pm_dry_run_verifies_and_reports_but_writes_nothing_and_logs_nothing() {
    let r = rig();
    let mut c = ctx(&r, Ok("pm"));
    assert!(ok(run(&mut c, r.candidate.clone(), true)));
    let (out, err) = c.captured();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!(
            "DRY RUN - nothing written. {} verified: lane-restart 9.9.9, sha256 {}; the installed exe would be kept as nothing was installed before\n",
            r.candidate.display(),
            sha_of_candidate()
        )
    );
    assert!(!r.hooks.join(EXE_NAME).exists());
    assert!(
        !r.state.join("restart.log").exists(),
        "a dry run writes no ledger row"
    );
}

#[test]
fn the_pm_install_swaps_the_exe_in_keeps_the_old_one_and_records_it() {
    let r = rig();
    // An exe is already installed: it must be kept, not lost.
    std::fs::write(r.hooks.join(EXE_NAME), b"the previous exe").unwrap();
    let mut c = ctx(&r, Ok("pm"));
    assert!(ok(run(&mut c, r.candidate.clone(), false)));
    let (out, err) = c.captured();
    assert_eq!(err, "");
    let bak = r.hooks.join(BAK_NAME);
    assert_eq!(
        out,
        format!(
            "INSTALLED lane-restart 9.9.9 at {}; sha256 {}; the previous exe is {}; {} rewritten\n",
            r.hooks.join(EXE_NAME).display(),
            sha_of_candidate(),
            bak.display(),
            r.hooks.join(SHA_NAME).display()
        )
    );
    assert_eq!(
        std::fs::read(r.hooks.join(EXE_NAME)).unwrap(),
        CANDIDATE_BYTES
    );
    assert_eq!(std::fs::read(&bak).unwrap(), b"the previous exe");
    let rows = rows(&r);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].acted);
    assert_eq!(rows[0].requested_by, "pm");
    assert!(
        rows[0].outcome.starts_with(&format!(
            "installed 9.9.9 sha256 {} from ",
            sha_of_candidate()
        )),
        "{}",
        rows[0].outcome
    );
}

#[test]
fn a_candidate_that_is_not_a_file_is_refused_and_the_refusal_is_recorded() {
    let r = rig();
    let mut c = ctx(&r, Ok("pm"));
    let missing = r.hooks.join("no-such-build.exe");
    assert!(failed(run(&mut c, missing.clone(), false)));
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert_eq!(
        err,
        format!(
            "lane-restart install: refused: {} is not a file: nothing to install\n",
            missing.display()
        )
    );
    let rows = rows(&r);
    assert!(!rows[0].acted);
    assert_eq!(rows[0].requested_by, "pm");
    assert!(
        rows[0].outcome.starts_with("refused: "),
        "{}",
        rows[0].outcome
    );
    assert!(!r.hooks.join(EXE_NAME).exists());
}

#[test]
fn the_command_line_reaches_it_through_run_with_and_bad_arguments_do_not() {
    let r = rig();
    let mut c = ctx(&r, Ok("pm"));
    let cand = r.candidate.to_string_lossy().into_owned();
    assert!(ok(run_with(
        &mut c,
        args(&["install", "--from", &cand, "--dry-run"])
    )));
    assert!(c.captured().0.starts_with("DRY RUN - nothing written."));
    // The accepted call above is the control for these refusals.
    for bad in [&["install", "--bogus"][..], &["install"][..]] {
        let mut c = ctx(&r, Ok("pm"));
        assert!(failed(run_with(&mut c, args(bad))), "{bad:?}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with("lane-restart install: "), "{err}");
    }
    assert!(!r.hooks.join(EXE_NAME).exists());
}
