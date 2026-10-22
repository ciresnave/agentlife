// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the service-lookup tests of `scripts/gh-runner-setup.ps1`: the runner truncates long service names, so the
//! service is found by the directory its executable lives in. Pure: fake service objects, no elevation. Windows only.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_runner_service_is_found_by_its_directory_not_by_a_computed_name() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-service.tests.ps1"
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
