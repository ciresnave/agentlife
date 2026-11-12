// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the -Rearm plan tests of `scripts/gh-runner-setup.ps1`: only a runner directory directly under the account's root,
//! whose service logs on as that account, is ever planned for deletion. Pure: temp dirs, fake services. Windows only.

#![cfg(windows)]

use std::process::Command;

#[test]
fn rearm_plans_only_a_runner_directly_under_its_own_root_for_its_own_account() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-rearm.tests.ps1"
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
