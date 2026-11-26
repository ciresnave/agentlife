// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the tests of `scripts/launcher-task-wrapper.ps1`: the generated `launcher-task.cmd` text (the portfolio lane-reporting
//! table set by default, the override, refused values), a dry run that writes nothing, and `-Write` with a dated backup. No
//! Synapse, no network, temp directory only. Windows only and PowerShell 7 (`pwsh`): a missing `pwsh` is a failure here.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_launcher_wrapper_script_sets_the_lane_reporting_table_and_refuses_bad_values() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\launcher-task-wrapper.tests.ps1"
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
