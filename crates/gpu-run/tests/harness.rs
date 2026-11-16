//! Runs scripts/gpu-run-native-test.sh (the bash harness) against the built exe. Windows only; it
//! takes the machine-wide `Global\gpu-run` mutex, so it must not run beside a real GPU job.
#![cfg(windows)]

use std::process::Command;

#[test]
fn bash_harness_passes() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/gpu-run-native-test.sh"
    );
    // On a dev box `bash` on PATH can be the WSL launcher, not Git Bash: prefer Git for Windows.
    let bash = std::env::var("GPU_RUN_TEST_BASH").ok().unwrap_or_else(|| {
        let git = "C:/Program Files/Git/bin/bash.exe";
        if std::path::Path::new(git).exists() {
            git.to_string()
        } else {
            "bash".to_string()
        }
    });
    let out = Command::new(bash)
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_gpu-run"))
        .output()
        .expect("bash must be on PATH (Git for Windows)");
    let text = String::from_utf8_lossy(&out.stdout);
    println!("{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "harness failed:\n{text}");
    assert!(text.contains("ALL PASSED"), "no ALL PASSED line:\n{text}");
}
