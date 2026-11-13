// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the config.cmd argument tests of `scripts/gh-runner-setup.ps1`: the ephemeral flag is never folded into
//! the --replace string (live failure 2026-11-13). Pure: a stub config.cmd in a temp dir. Windows only.

#![cfg(windows)]

use std::process::Command;

#[test]
fn config_cmd_gets_replace_and_ephemeral_as_separate_arguments() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-ephemeral.tests.ps1"
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
