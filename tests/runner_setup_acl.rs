// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the ACL-preflight tests of `scripts/gh-runner-setup.ps1` (board 224) on real ACLs in a temp directory:
//! a runner account must not be able to write the lane tools that run as the owner with vault access. The
//! tests need no elevation. Windows only: the script and the ACLs are.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_runner_setup_acl_preflight_refuses_exposed_paths_and_passes_clean_ones() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-acl.tests.ps1"
    );
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            script,
        ])
        .output()
        .expect("run powershell.exe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && text.contains("all passed"),
        "exit {:?}\n{text}",
        out.status.code()
    );
}
