// SPDX-License-Identifier: MIT OR Apache-2.0

//! `attach --watch` against a REAL direct host (`docs/PORT-LINUX-PLAN.md`, P6b). Unix only.
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_watcher_sees_the_lane_and_cannot_type_into_it() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let state = home.path().join("state");
    let exe = env!("CARGO_BIN_EXE_lane-restart");

    // The lane: prints a marker, then waits for ONE byte of input on its tty and records it.
    let mut host = Command::new(exe)
        .args([
            "host",
            "--role",
            "p6btest",
            "--",
            "sh",
            "-c",
            "echo VISIBLE_P6B; head -c 1 > got.bin; echo AFTER_P6B",
        ])
        .current_dir(work.path())
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", &state)
        .env_remove("USERPROFILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the host");

    let sock = state.join("lane-state").join("watch").join("host-p6btest.sock");
    wait_for("the watch socket", 15, || sock.exists());

    // 1. the watcher sees the lane's screen
    let mut watcher = Command::new(exe)
        .args(["attach", "--role", "p6btest", "--watch"])
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", &state)
        .env_remove("USERPROFILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the watcher");
    let mut out = watcher.stdout.take().unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&seen).contains("VISIBLE_P6B") {
        assert!(Instant::now() < deadline, "no screen: {:?}", String::from_utf8_lossy(&seen));
        let n = out.read(&mut buf).unwrap();
        assert!(n > 0, "watcher closed: {:?}", String::from_utf8_lossy(&seen));
        seen.extend_from_slice(&buf[..n]);
    }

    // 2. what a watcher sends does not reach the lane: it is still waiting for its byte
    let mut raw = UnixStream::connect(&sock).unwrap();
    raw.write_all(b"X\n").unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !work.path().join("got.bin").exists(),
        "a watcher's bytes reached the lane"
    );
    let _ = watcher.kill();
    let _ = watcher.wait();
    let _ = host.kill();
    let _ = host.wait();
}

/// Positive control for the test above: the same lane DOES record a byte that arrives through the
/// host's own input path, so "got.bin never appeared" there means the watcher's bytes were not
/// delivered, not that this lane cannot record anything.
#[test]
fn the_same_lane_records_a_byte_sent_through_the_hosts_own_stdin() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let exe = env!("CARGO_BIN_EXE_lane-restart");
    let mut host = Command::new(exe)
        .args([
            "host",
            "--role",
            "p6bctl",
            "--",
            "sh",
            "-c",
            "echo VISIBLE_P6B; head -c 1 > got.bin; echo AFTER_P6B",
        ])
        .current_dir(work.path())
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env_remove("USERPROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the host");
    let mut stdin = host.stdin.take().unwrap();
    // the lane has to be at `head` before the byte is useful; resend until it is recorded
    let got = work.path().join("got.bin");
    wait_for("the byte to be recorded", 15, || {
        let _ = stdin.write_all(b"Y");
        let _ = stdin.flush();
        got.exists() && std::fs::metadata(&got).map(|m| m.len() >= 1).unwrap_or(false)
    });
    drop(stdin);
    let _ = host.kill();
    let _ = host.wait();
}

#[test]
fn attach_without_watch_is_refused_and_says_why() {
    let out = Command::new(env!("CARGO_BIN_EXE_lane-restart"))
        .args(["attach", "--role", "anyrole"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--watch is required"), "{err}");
    assert!(err.contains("interactive attach is not built"), "{err}");
}

#[test]
fn attach_to_a_role_with_no_host_fails_with_a_clear_message() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lane-restart"))
        .args(["attach", "--role", "nobody", "--watch"])
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env_remove("USERPROFILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is a direct host running"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
