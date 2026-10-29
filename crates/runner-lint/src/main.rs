// SPDX-License-Identifier: MIT OR Apache-2.0

//! `runner-lint [FILE...]`: exit 0 when no job that can run on a self-hosted runner is reachable from a pull
//! request or an unlisted trigger without the same-repo condition and an environment, 1 when one is, 2 when
//! a file cannot be read. With no file arguments it lints `git ls-files` of the workflow directory (the index,
//! never a walk of the disk) and says so.

use std::process::{Command, ExitCode};

/// A repo is linted as private only when GitHub says it is: a public repo run with `--private` would pass
/// pull-request jobs that have no environment.
fn prove_private() -> Result<(), String> {
    let repo = std::env::var("GITHUB_REPOSITORY")
        .map_err(|_| "GITHUB_REPOSITORY is not set (owner/name)".to_string())?;
    if repo.is_empty() || !repo.contains('/') || repo.contains(char::is_whitespace) {
        return Err(format!("GITHUB_REPOSITORY {repo:?} is not owner/name"));
    }
    let out = Command::new("gh")
        .args(["api", &format!("repos/{repo}"), "--jq", ".private"])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && said == "true" {
        Ok(())
    } else {
        Err(format!(
            "gh api repos/{repo} --jq .private did not say true (said {said:?}, exit {:?}, stderr {:?})",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn main() -> ExitCode {
    let mut files: Vec<String> = std::env::args().skip(1).collect();
    if files.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: runner-lint [--private] [FILE...]   (no FILE: git ls-files '.github/workflows/*.yml' '.github/workflows/*.yaml')\n  --private  lint as a PRIVATE repo (a pull-request runner job needs the same-repo `if` but no environment).\n             Proven, not trusted: needs GITHUB_REPOSITORY=owner/name and `gh` (GH_TOKEN), and exits 2 unless\n             `gh api repos/$GITHUB_REPOSITORY --jq .private` says true.");
        return ExitCode::SUCCESS;
    }
    if files.iter().any(|a| a == "-V" || a == "--version") {
        println!("runner-lint {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut mode = runner_lint::Mode::Public;
    if let Some(i) = files.iter().position(|a| a == "--private") {
        files.remove(i);
        match prove_private() {
            Ok(()) => mode = runner_lint::Mode::Private,
            Err(e) => {
                eprintln!("runner-lint: --private refused: {e}");
                return ExitCode::from(2);
            }
        }
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
        let r = runner_lint::lint_as(f, &text, mode);
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
