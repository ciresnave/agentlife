// SPDX-License-Identifier: MIT OR Apache-2.0
//! OPS-011 cli-basics: `--version` prints the crate version and exits 0; a bad flag exits 2.
use std::process::Command;

#[test]
fn version_prints_and_a_bad_flag_exits_two() {
    let exe = env!("CARGO_BIN_EXE_pin-plan");
    let v = Command::new(exe).arg("--version").output().unwrap();
    assert_eq!(v.status.code(), Some(0));
    let out = String::from_utf8_lossy(&v.stdout);
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    let short = Command::new(exe).arg("-V").output().unwrap();
    assert_eq!(short.stdout, v.stdout);
    let bad = Command::new(exe).arg("--no-such-flag").output().unwrap();
    assert_eq!(bad.status.code(), Some(2), "{bad:?}");
}
