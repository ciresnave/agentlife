// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ledger` through the `Ctx` seam: a `restart.log` written into the test's own state directory
//! with the real `log::append`, and the text the command printed.

use super::*;
use crate::app::run_with;
use crate::log::{append, Entry};
use chrono::TimeZone;
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

fn ctx_in(dir: &Path) -> Ctx {
    Ctx::capture(dir.join("state"), Ok(dir.to_path_buf()), None)
}

/// Three entries: alpha acted (restart), alpha refused (start), beta acted (stop); 2026-10-14.
fn write_log(state: &Path) -> PathBuf {
    let path = state.join("restart.log");
    let at = |h| chrono::Utc.with_ymd_and_hms(2026, 10, 14, h, 0, 0).unwrap();
    for (h, role, kind, by, acted, outcome) in [
        (10, "alpha", None, "self", true, "killed pid 7"),
        (11, "alpha", Some("start"), "pm", false, "refused: busy"),
        (12, "beta", Some("stop"), "pm", true, "stopped"),
    ] {
        append(
            &path,
            &Entry {
                timestamp: at(h),
                requested_by: by,
                role,
                outcome: outcome.to_string(),
                acted,
                kind,
                caller_role: None,
                model: None,
                reason: None,
                signal_id: None,
                result: None,
                spawned: None,
            },
        )
        .unwrap();
    }
    path
}

fn filter() -> ledger::Filter {
    ledger::Filter::default()
}

#[test]
fn a_missing_log_is_an_empty_ledger_not_an_error() {
    let d = tempfile::tempdir().unwrap();
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), false, None, false)));
    let (out, err) = ctx.captured();
    assert_eq!(out, "(0 of 0 entries shown; 0 unreadable line(s))\n");
    assert_eq!(err, "");
}

#[test]
fn the_text_view_prints_one_row_per_entry_oldest_first_and_a_footer() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), false, None, false)));
    let out = ctx.captured().0;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert_eq!(
        lines[0],
        "2026-10-14T10:00:00Z\talpha\trestart\tself\tacted\tkilled pid 7"
    );
    assert_eq!(
        lines[1],
        "2026-10-14T11:00:00Z\talpha\tstart\tpm\trefused\trefused: busy"
    );
    assert_eq!(
        lines[2],
        "2026-10-14T12:00:00Z\tbeta\tstop\tpm\tacted\tstopped"
    );
    assert_eq!(lines[3], "(3 of 3 entries shown; 0 unreadable line(s))");
}

#[test]
fn filters_by_role_and_by_acted_and_last_select_the_rows_asked_for() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let count = |f: ledger::Filter| {
        let mut ctx = ctx_in(d.path());
        assert!(ok(run(&mut ctx, f, false, None, false)));
        ctx.captured().0.lines().count() - 1
    };
    assert_eq!(
        count(ledger::Filter {
            role: Some("alpha".into()),
            ..filter()
        }),
        2
    );
    assert_eq!(
        count(ledger::Filter {
            acted: Some(false),
            ..filter()
        }),
        1
    );
    assert_eq!(
        count(ledger::Filter {
            last: Some(1),
            ..filter()
        }),
        1
    );
}

#[test]
fn since_keeps_only_newer_entries_and_a_bad_since_is_named_and_fails() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(
        &mut ctx,
        filter(),
        false,
        Some("2026-10-14T10:30:00Z".into()),
        false
    )));
    let out = ctx.captured().0;
    assert!(
        out.ends_with("(2 of 3 entries shown; 0 unreadable line(s))\n"),
        "{out}"
    );
    // The control: a bad span is a named refusal, not an empty list.
    let mut ctx = ctx_in(d.path());
    assert!(failed(run(
        &mut ctx,
        filter(),
        false,
        Some("soon".into()),
        false
    )));
    let (out, err) = ctx.captured();
    assert_eq!(out, "");
    assert!(
        err.starts_with("lane-restart ledger: --since takes an RFC 3339 time or a span"),
        "{err}"
    );
}

#[test]
fn json_prints_one_object_per_line_on_stdout_and_the_footer_on_stderr() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), true, None, false)));
    let (out, err) = ctx.captured();
    let rows: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .collect();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1]["role"], "alpha");
    assert_eq!(rows[1]["acted"], false);
    assert_eq!(err, "(3 of 3 entries shown; 0 unreadable line(s))\n");
}

#[test]
fn by_lane_counts_acted_and_refused_per_role() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), false, None, true)));
    assert_eq!(
        ctx.captured().0,
        "role\tacted\trefused\nalpha\t1\t1\nbeta\t1\t0\n(2 role(s); 3 of 3 entries counted)\n"
    );
}

#[test]
fn an_unreadable_line_is_counted_and_named_never_dropped_silently() {
    let d = tempfile::tempdir().unwrap();
    let path = write_log(&d.path().join("state"));
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("not json at all\n");
    std::fs::write(&path, text).unwrap();
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), false, None, false)));
    let out = ctx.captured().0;
    assert!(
        out.contains("(3 of 3 entries shown; 1 unreadable line(s): line 4 ("),
        "{out}"
    );
}

#[test]
fn the_log_is_read_from_the_contexts_state_directory_only() {
    // A log in the test's OTHER directory is not read: the path comes from the context.
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("elsewhere"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run(&mut ctx, filter(), false, None, false)));
    assert_eq!(
        ctx.captured().0,
        "(0 of 0 entries shown; 0 unreadable line(s))\n"
    );
}

#[test]
fn the_command_line_reaches_it_through_run_with_and_bad_arguments_do_not() {
    let d = tempfile::tempdir().unwrap();
    write_log(&d.path().join("state"));
    let mut ctx = ctx_in(d.path());
    assert!(ok(run_with(
        &mut ctx,
        args(&["ledger", "--role", "alpha", "--acted"])
    )));
    // One row: alpha's acted entry; the accepted call is the control for the refusals below.
    let out = ctx.captured().0;
    assert!(
        out.ends_with("(1 of 3 entry shown; 0 unreadable line(s))\n")
            || out.ends_with("(1 of 3 entries shown; 0 unreadable line(s))\n"),
        "{out}"
    );
    assert!(out.starts_with("2026-10-14T10:00:00Z\talpha\t"), "{out}");
    for bad in [&["ledger", "--bogus"][..], &["ledger", "--last", "x"][..]] {
        let mut ctx = ctx_in(d.path());
        assert!(failed(run_with(&mut ctx, args(bad))), "{bad:?}");
        let (out, err) = ctx.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with("lane-restart ledger: "), "{err}");
    }
}
