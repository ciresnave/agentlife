// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the runner `.env` and probe tests of `scripts/gh-runner-setup.ps1`: Git bash ahead of the WSL launcher, a Rust toolchain,
//! and the probe verdict (mlmf first job, 2026-11-12). Pure functions plus one real-process PATH check. Windows only.

#![cfg(windows)]

use std::process::Command;

#[test]
fn the_runner_env_puts_git_bash_before_the_wsl_launcher_and_gives_jobs_rust_and_the_probe_judges_it() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        r"\scripts\tests\gh-runner-setup-env.tests.ps1"
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
