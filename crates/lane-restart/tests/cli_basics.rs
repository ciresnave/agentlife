//! OPS-011 cli-basics: `--version` prints the crate version and exits 0; a bad flag exits 1 (2 is taken in this binary).
use std::process::Command;

#[test]
fn version_prints_and_a_bad_flag_exits_one() {
    let exe = env!("CARGO_BIN_EXE_lane-restart");
    let v = Command::new(exe).arg("--version").output().unwrap();
    assert_eq!(v.status.code(), Some(0));
    let out = String::from_utf8_lossy(&v.stdout);
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    let short = Command::new(exe).arg("-V").output().unwrap();
    assert_eq!(short.stdout, v.stdout);
    let bad = Command::new(exe).arg("--no-such-flag").output().unwrap();
    // Not 2: 2 means "relaunched, awaiting a human at the dialog" (`lane-restart --help`); scripts branch on it.
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
}
