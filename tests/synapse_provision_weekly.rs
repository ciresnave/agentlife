// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the tests of `scripts/synapse-provision-weekly.ps1` against a fake `synapse`: dry run by default, the
//! environment it hands the child, counts, exit 2 on a refused role, fail-closed on empty or short output, nothing
//! echoed, unsafe role names, and the announce-file copy. No Synapse, no network. Windows only (the fake is a `.cmd`)
//! and PowerShell 7 (`pwsh`): a missing `pwsh` is a failure here, not a skip.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_weekly_provision_script_counts_fails_closed_and_copies_the_announce_file() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\synapse-provision-weekly.tests.ps1"
    );
    let out = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-File", script])
        .output()
        .expect("run pwsh (PowerShell 7 must be on PATH)");
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
