// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the tests of `scripts/github-app-setup.ps1` (roadmap C1) against a local stand-in for GitHub: the
//! manifest page, the one-time `state`, the code exchange, DPAPI storage, the key never being printed, and
//! `-Verify` with its known-bad controls. No elevation, no network, no GitHub account. Windows only (DPAPI) and
//! PowerShell 7 (`pwsh`): a missing `pwsh` is a failure here, not a skip.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_github_app_setup_script_creates_stores_and_verifies_without_leaking_the_key() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\github-app-setup.tests.ps1"
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
