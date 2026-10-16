// SPDX-License-Identifier: MIT OR Apache-2.0

//! `runner-lint [FILE...]`: exit 0 when no job that can run on a self-hosted runner is reachable from a pull
//! request or an unlisted trigger without the same-repo condition and an environment, 1 when one is, 2 when
//! a file cannot be read. With no file arguments it lints `git ls-files` of the workflow directory (the index,
//! never a walk of the disk) and says so.

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut files: Vec<String> = std::env::args().skip(1).collect();
    if files.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: runner-lint [FILE...]   (no FILE: git ls-files '.github/workflows/*.yml' '.github/workflows/*.yaml')");
        return ExitCode::SUCCESS;
    }
    if files.is_empty() {
        let out = Command::new("git")
            .args([
                "ls-files",
                ".github/workflows/*.yml",
                ".github/workflows/*.yaml",
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                files = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::to_string)
                    .collect();
                eprintln!(
                    "runner-lint: enumerated {} workflow file(s) with git ls-files",
                    files.len()
                );
            }
            Ok(o) => {
                eprintln!(
                    "runner-lint: git ls-files failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
                return ExitCode::from(2);
            }
            Err(e) => {
                eprintln!("runner-lint: cannot run git: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let (mut jobs, mut runner_jobs, mut bad) = (0, 0, 0);
    for f in &files {
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("runner-lint: cannot read {f}: {e}");
                return ExitCode::from(2);
            }
        };
        let r = runner_lint::lint(f, &text);
        jobs += r.jobs;
        runner_jobs += r.runner_jobs;
        for finding in &r.findings {
            println!("{finding}");
            bad += 1;
        }
    }
    eprintln!(
        "runner-lint: {} file(s), {jobs} job(s), {runner_jobs} able to run on a self-hosted runner, {bad} finding(s)",
        files.len()
    );
    if bad > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
