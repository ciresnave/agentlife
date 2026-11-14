// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the job-completed cleanup tests (CireSnave 2026-11-14: runners must clean up after every job): the hook as a real process
//! against a fake runner dir, and the apply script's `.env` editing. Windows only.

#![cfg(windows)]

use std::process::Command;

fn run_ps1(rel: &str) {
    let script = format!(r"{}\{rel}", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &script,
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

#[test]
fn the_hook_deletes_what_a_job_leaves_keeps_the_runner_and_refuses_a_non_runner_dir() {
    run_ps1(r"scripts\tests\runner-job-cleanup-hook.tests.ps1");
}

#[test]
fn the_apply_script_edits_env_idempotently_never_prints_it_and_restarts_only_idle_listeners() {
    run_ps1(r"scripts\tests\runner-cleanup-apply.tests.ps1");
}
