// SPDX-License-Identifier: MIT OR Apache-2.0

//! `host` through the `Ctx` seam: a fake host that records what it was asked to run and returns a
//! chosen outcome (no ConPTY, no `claude`), and a tempdir state directory for the boot record. The
//! machine's state directory is never touched.

use super::*;
use crate::app::run_with;
use crate::host::HostOutcome;
use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

type Asked = Rc<RefCell<Vec<(String, Vec<String>)>>>;

fn code_is(code: ExitCode, ok: bool) -> bool {
    let want = if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    format!("{code:?}") == format!("{want:?}")
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

fn outcome(code: Option<u32>) -> std::io::Result<HostOutcome> {
    Ok(HostOutcome {
        child_exit_code: code,
        abandoned_threads: Vec::new(),
        reader_end_after_exit: None,
    })
}

/// A context over `state` whose host records the request and answers `answer`.
fn ctx(state: &Path, asked: &Asked, answer: fn() -> std::io::Result<HostOutcome>) -> Ctx {
    let asked = Rc::clone(asked);
    Ctx::capture(state.to_path_buf(), Ok(state.to_path_buf()), None).with_host_runner(Rc::new(
        move |role, child| {
            asked.borrow_mut().push((role.to_string(), child.to_vec()));
            answer()
        },
    ))
}

fn boot_records(state: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(state)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("host-boot-") && n.ends_with(".txt"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn the_host_runs_the_child_it_was_given_and_a_clean_exit_is_success() {
    let d = tempfile::tempdir().unwrap();
    let asked: Asked = Rc::default();
    let mut c = ctx(d.path(), &asked, || outcome(Some(0)));
    let code = run_with(
        &mut c,
        args(&[
            "host", "--role", "alpha", "--", "claude", "--name", "alpha", "--model", "sonnet",
        ]),
    );
    assert!(code_is(code, true), "{code:?}");
    assert_eq!(
        *asked.borrow(),
        vec![(
            "alpha".to_string(),
            ["claude", "--name", "alpha", "--model", "sonnet"]
                .map(String::from)
                .to_vec()
        )]
    );
    assert_eq!(c.captured(), (String::new(), String::new()));
}

#[test]
fn the_boot_record_is_written_first_into_the_state_dir_with_the_hosts_own_arguments() {
    let d = tempfile::tempdir().unwrap();
    let asked: Asked = Rc::default();
    let mut c = ctx(d.path(), &asked, || outcome(Some(0)));
    run_with(
        &mut c,
        args(&["host", "--role", "alpha", "--", "claude", "--x"]),
    );
    let records = boot_records(d.path());
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0],
        format!("host-boot-{}.txt", std::process::id()),
        "named for this process"
    );
    let body = std::fs::read_to_string(d.path().join(&records[0])).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        &lines[1..],
        ["--role", "alpha", "--", "claude", "--x"],
        "the host's own arguments after the program name and `host`: {body}"
    );
}

#[test]
fn a_child_that_exits_nonzero_is_failure_and_one_that_reports_no_code_is_success() {
    for (code, ok) in [(Some(3), false), (Some(0), true), (None, true)] {
        let d = tempfile::tempdir().unwrap();
        let asked: Asked = Rc::default();
        let answer: fn() -> std::io::Result<HostOutcome> = match code {
            Some(3) => || outcome(Some(3)),
            Some(_) => || outcome(Some(0)),
            None => || outcome(None),
        };
        let mut c = ctx(d.path(), &asked, answer);
        let got = run_with(&mut c, args(&["host", "--role", "alpha", "--", "claude"]));
        assert!(code_is(got, ok), "exit {code:?}: {got:?}");
    }
}

#[test]
fn a_host_that_cannot_run_says_so_on_stderr_and_fails() {
    let d = tempfile::tempdir().unwrap();
    let asked: Asked = Rc::default();
    let mut c = ctx(d.path(), &asked, || {
        Err(std::io::Error::other("no pty on this box"))
    });
    let code = run_with(&mut c, args(&["host", "--role", "alpha", "--", "claude"]));
    assert!(code_is(code, false), "{code:?}");
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert_eq!(err, "lane-restart host: no pty on this box\n");
}

#[test]
fn wrong_arguments_still_leave_the_boot_record_and_never_start_a_host() {
    let d = tempfile::tempdir().unwrap();
    let asked: Asked = Rc::default();
    let mut c = ctx(d.path(), &asked, || outcome(Some(0)));
    let code = run_with(&mut c, args(&["host", "--bogus"]));
    assert!(code_is(code, false), "{code:?}");
    let (out, err) = c.captured();
    assert_eq!(out, "");
    assert!(err.starts_with("lane-restart host: "), "{err}");
    assert!(asked.borrow().is_empty(), "a host was started");
    assert_eq!(boot_records(d.path()).len(), 1, "no boot record");
}
