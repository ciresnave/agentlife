// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the account tests of `scripts/gh-runner-setup.ps1` (docs/RUNNER-ACCOUNTS.md T1, T2, T4, T5): the account name derived
//! from the repo, an existing account with services refused unless `-RotateShared`, groups listed and never defaulted, and a
//! dry run that creates no account and no directory. Fake service objects, no elevation. Windows only.

#![cfg(windows)]

use std::process::Command;

#[test]
fn one_account_per_runner_add_only_groups_listed_and_a_dry_run_changes_nothing() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-account.tests.ps1"
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
