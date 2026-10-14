// SPDX-License-Identifier: MIT OR Apache-2.0

//! The headless host (`docs/PORT-LINUX-PLAN.md`, P6): started with no terminal, it runs its child
//! and makes itself a session leader so its launcher's terminal cannot take it down. Linux only:
//! it reads `/proc`.
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The session id (field 6 of `/proc/<pid>/stat`, read after the last `)`).
fn session_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(3)?.parse().ok()
}

#[test]
fn a_host_started_without_a_tty_becomes_its_own_session_leader() {
    let home = tempfile::tempdir().unwrap();
    let exe = env!("CARGO_BIN_EXE_lane-restart");
    let mut host = Command::new(exe)
        .args(["host", "--role", "p6test", "--", "sh", "-c", "sleep 4"])
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env_remove("USERPROFILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the host");
    let pid = host.id();
    // control: before it has run `setsid`, it shares the test's session
    let mine = session_of(std::process::id()).expect("our own session");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut sid = None;
    while Instant::now() < deadline {
        sid = session_of(pid);
        if sid == Some(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = host.kill();
    let _ = host.wait();
    assert_ne!(mine, pid, "control: the test is not the host's session");
    assert_eq!(sid, Some(pid), "a headless host must lead its own session");
}
