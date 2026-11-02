// SPDX-License-Identifier: MIT OR Apache-2.0

//! `attach` through the `Ctx` seam. The refusals are the same on every OS; what a watch does is
//! per OS (a unix socket off Windows, a plain refusal on Windows), so those tests are gated.

use super::*;
use crate::app::run_with;
use std::ffi::OsString;

#[cfg(unix)]
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

fn ctx_in(dir: &std::path::Path) -> Ctx {
    Ctx::capture(dir.join("state"), Ok(dir.to_path_buf()), None)
}

fn refuse(rest: &[&str]) -> (String, String) {
    let d = tempfile::tempdir().unwrap();
    let mut ctx = ctx_in(d.path());
    assert!(failed(run_with(&mut ctx, args(rest))), "{rest:?}");
    ctx.captured()
}

#[test]
fn a_bad_flag_is_named_by_the_parser_on_stderr() {
    let (out, err) = refuse(&["attach", "--bogus"]);
    assert_eq!(out, "");
    assert!(err.starts_with("lane-restart attach: "), "{err}");
}

#[test]
fn a_missing_or_unsafe_role_is_refused_before_anything_else() {
    for rest in [
        &["attach", "--watch"][..],
        &["attach", "--role", "a;b", "--watch"][..],
        &["attach", "--role", "has space", "--watch"][..],
    ] {
        let (out, err) = refuse(rest);
        assert_eq!(out, "", "{rest:?}");
        assert_eq!(
            err, "lane-restart attach: --role <name> is required and must be a plain identifier\n",
            "{rest:?}"
        );
    }
}

#[test]
fn without_watch_it_says_only_read_only_watching_exists() {
    let (out, err) = refuse(&["attach", "--role", "alpha"]);
    assert_eq!(out, "");
    assert!(
        err.starts_with(
            "lane-restart attach: --watch is required. Only read-only watching exists;"
        ),
        "{err}"
    );
    assert!(err.contains("interactive attach is not built"), "{err}");
}

#[cfg(not(unix))]
#[test]
fn off_unix_a_valid_watch_is_refused_as_not_available_on_this_os() {
    let (out, err) = refuse(&["attach", "--role", "alpha", "--watch"]);
    assert_eq!(out, "");
    assert!(
        err.starts_with("lane-restart attach: not available on this OS"),
        "{err}"
    );
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    #[test]
    fn with_no_host_running_it_names_the_socket_and_asks_if_a_host_is_running() {
        let d = tempfile::tempdir().unwrap();
        let mut ctx = ctx_in(d.path());
        assert!(failed(run_with(
            &mut ctx,
            args(&["attach", "--role", "alpha", "--watch"])
        )));
        let (out, err) = ctx.captured();
        assert_eq!(out, "");
        assert!(
            err.starts_with("lane-restart attach: watching alpha (read-only; the host closes this when it ends)\n"),
            "{err}"
        );
        assert!(err.contains("host-alpha.sock"), "{err}");
        assert!(
            err.trim_end()
                .ends_with("(is a direct host running for alpha?)"),
            "{err}"
        );
    }

    #[test]
    fn a_running_host_s_screen_is_copied_to_stdout_until_it_closes() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let path = crate::watch::socket_path(&state, "alpha");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        let host = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"SCREEN ").unwrap();
            s.write_all(b"AND MORE").unwrap();
        });
        let mut ctx = ctx_in(d.path());
        assert!(ok(run_with(
            &mut ctx,
            args(&["attach", "--role", "alpha", "--watch"])
        )));
        host.join().unwrap();
        let (out, err) = ctx.captured();
        assert_eq!(out, "SCREEN AND MORE");
        assert_eq!(
            err,
            "lane-restart attach: watching alpha (read-only; the host closes this when it ends)\n"
        );
    }
}
