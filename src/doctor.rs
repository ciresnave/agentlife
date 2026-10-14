// SPDX-License-Identifier: MIT OR Apache-2.0
//! `agentlife doctor`: is what is installed what this build needs? (RESTORE-GAP-ANALYSIS R16, R17.)
//!
//! Read-only, and honest about what it could not read: an installed program that cannot be run is
//! `UNREADABLE`, never "ok", and nothing is installed, copied or repaired. The checks:
//!
//! * **R16, the host.** The configured `host_program` (`lane-restart`) is resolved the way a task would
//!   resolve it, run with `--version`, and compared with the version this build requires. The workspace
//!   has one version number, so the `lane-restart` this build was made with is `agentlife::version()`;
//!   agentlife has no finer pin, and the line says so.
//! * **R17, the hook binary.** `agentlife.exe` at the path the hooks and the tasks use
//!   (`HOOK_EXE`, from `docs/HOOK-INSTALL.md`, or `--exe`): present, and what version it reports.
//!
//! The probes are injected so the verdicts are testable without a machine that has either installed.

use std::path::{Path, PathBuf};

/// Where `docs/HOOK-INSTALL.md` tells the hooks and the logon task to find this binary on
/// Windows. Off Windows see [`hook_exe`].
pub const HOOK_EXE: &str = "C:/Projects/.claude-hooks/agentlife.exe";

/// The hook binary's path on this OS: [`HOOK_EXE`] on Windows, `agentlife` in the hooks directory
/// elsewhere (`lane_state::platform::hooks_dir`).
pub fn hook_exe() -> String {
    if cfg!(windows) {
        HOOK_EXE.to_string()
    } else {
        lane_state::platform::hooks_dir()
            .join("agentlife")
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Found, runs, and reports a different version from the one this build requires.
    Mismatch,
    /// Not found.
    Missing,
    /// Found, but its version could not be read (it did not run, or printed something unexpected).
    Unreadable,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "OK",
            Verdict::Mismatch => "MISMATCH",
            Verdict::Missing => "MISSING",
            Verdict::Unreadable => "UNREADABLE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub check: &'static str,
    pub verdict: Verdict,
    pub detail: String,
}

/// What the checks read from the machine.
pub struct Probe<'a> {
    /// A program name or path to the file it names (`None`: not found).
    pub resolve: &'a dyn Fn(&str) -> Option<PathBuf>,
    /// The first line a program prints for `--version`.
    pub version_line: &'a dyn Fn(&Path) -> Result<String, String>,
}

/// `"lane-restart 0.7.1"` with `tool` `"lane-restart"` is `Some("0.7.1")`.
pub fn parse_version(line: &str, tool: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    if words.next()? != tool {
        return None;
    }
    let v = words.next()?;
    let numeric = v.split('.').count() >= 2
        && v.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    numeric.then(|| v.to_string())
}

fn triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next().unwrap_or(Some(0))?))
}

fn how_different(found: &str, required: &str) -> &'static str {
    match (triple(found), triple(required)) {
        (Some(f), Some(r)) if f < r => "older than",
        (Some(f), Some(r)) if f > r => "newer than",
        _ => "different from",
    }
}

fn check_program(
    check: &'static str,
    tool: &str,
    shown_as: &str,
    path: Option<PathBuf>,
    required: &str,
    probe: &Probe,
    note: &str,
) -> Line {
    let Some(path) = path else {
        return Line {
            check,
            verdict: Verdict::Missing,
            detail: format!("{shown_as}: not found"),
        };
    };
    let said = match (probe.version_line)(&path) {
        Ok(l) => l,
        Err(e) => {
            return Line {
                check,
                verdict: Verdict::Unreadable,
                detail: format!("{}: could not read its version: {e}", path.display()),
            }
        }
    };
    let Some(found) = parse_version(&said, tool) else {
        return Line {
            check,
            verdict: Verdict::Unreadable,
            detail: format!(
                "{}: expected `{tool} <version>` from --version, got {said:?}",
                path.display()
            ),
        };
    };
    let (verdict, relation) = if found == required {
        (Verdict::Ok, "the version this build requires".to_string())
    } else {
        (
            Verdict::Mismatch,
            format!(
                "{} the {required} this build requires",
                how_different(&found, required)
            ),
        )
    };
    Line {
        check,
        verdict,
        detail: format!("{}: {found}, {relation}{note}", path.display()),
    }
}

/// The checks, in order. `host_program` is the configured name or path; `hook_exe` is where the hooks
/// look for this binary; `required` is the version this build needs of both.
pub fn run(host_program: &str, hook_exe: &str, required: &str, probe: &Probe) -> Vec<Line> {
    vec![
        check_program(
            "R16 host (lane-restart)",
            "lane-restart",
            host_program,
            (probe.resolve)(host_program),
            required,
            probe,
            " (the workspace has one version number; agentlife pins no finer host version)",
        ),
        check_program(
            "R17 hook binary (agentlife)",
            "agentlife",
            hook_exe,
            (probe.resolve)(hook_exe),
            required,
            probe,
            " (the path the hooks and the logon task use)",
        ),
    ]
}

/// Plain text for a person. Ends with what was *not* done, so a clean run is not read as an install.
pub fn render(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&format!(
            "{:<10} {}\n           {}\n",
            l.verdict.label(),
            l.check,
            l.detail
        ));
    }
    out.push_str("Read-only: nothing was installed, copied or changed.\n");
    out
}

pub fn all_ok(lines: &[Line]) -> bool {
    lines.iter().all(|l| l.verdict == Verdict::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    type Says = [(&'static str, Result<&'static str, &'static str>)];

    fn lines(files: &[(&'static str, &'static str)], says: &Says) -> Vec<Line> {
        let files: HashMap<_, _> = files.iter().cloned().collect();
        let says: HashMap<_, _> = says.iter().cloned().collect();
        let resolve = |n: &str| files.get(n).map(PathBuf::from);
        let version_line = |p: &Path| match says.get(p.to_str().unwrap()) {
            Some(Ok(s)) => Ok(s.to_string()),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("no such probe".into()),
        };
        run(
            "lane-restart",
            "C:/h/agentlife.exe",
            "0.12.3",
            &Probe {
                resolve: &resolve,
                version_line: &version_line,
            },
        )
    }

    const BOTH: [(&str, &str); 2] = [
        ("lane-restart", "C:/b/lane-restart.exe"),
        ("C:/h/agentlife.exe", "C:/h/agentlife.exe"),
    ];

    #[test]
    fn parses_only_the_tool_it_asked_about() {
        assert_eq!(
            parse_version("lane-restart 0.7.1", "lane-restart").as_deref(),
            Some("0.7.1")
        );
        assert_eq!(parse_version("agentlife 0.7.1", "lane-restart"), None);
        assert_eq!(parse_version("lane-restart", "lane-restart"), None);
        assert_eq!(parse_version("lane-restart vNext", "lane-restart"), None);
        assert_eq!(parse_version("", "lane-restart"), None);
    }

    #[test]
    fn matching_versions_are_ok_and_say_which_path_was_read() {
        let l = lines(
            &BOTH,
            &[
                ("C:/b/lane-restart.exe", Ok("lane-restart 0.12.3")),
                ("C:/h/agentlife.exe", Ok("agentlife 0.12.3")),
            ],
        );
        assert!(all_ok(&l));
        assert!(l[0].detail.contains("C:/b/lane-restart.exe"));
        assert!(l[1].detail.contains("C:/h/agentlife.exe"));
    }

    #[test]
    fn an_older_host_is_a_mismatch_naming_both_versions() {
        let l = lines(
            &BOTH,
            &[
                ("C:/b/lane-restart.exe", Ok("lane-restart 0.7.1")),
                ("C:/h/agentlife.exe", Ok("agentlife 0.12.3")),
            ],
        );
        assert_eq!(l[0].verdict, Verdict::Mismatch);
        assert!(l[0].detail.contains("0.7.1"));
        assert!(l[0].detail.contains("older than the 0.12.3"));
        assert_eq!(l[1].verdict, Verdict::Ok);
        assert!(!all_ok(&l));
    }

    #[test]
    fn a_newer_hook_binary_is_a_mismatch_too() {
        let l = lines(
            &BOTH,
            &[
                ("C:/b/lane-restart.exe", Ok("lane-restart 0.12.3")),
                ("C:/h/agentlife.exe", Ok("agentlife 0.13.0")),
            ],
        );
        assert_eq!(l[1].verdict, Verdict::Mismatch);
        assert!(l[1].detail.contains("newer than"));
    }

    #[test]
    fn nothing_installed_is_missing_not_ok() {
        let l = lines(&[], &[]);
        assert_eq!(l[0].verdict, Verdict::Missing);
        assert_eq!(l[1].verdict, Verdict::Missing);
        assert!(l[1].detail.contains("C:/h/agentlife.exe"));
    }

    #[test]
    fn a_program_that_will_not_say_its_version_is_unreadable_not_ok() {
        let l = lines(
            &BOTH,
            &[
                ("C:/b/lane-restart.exe", Err("exit code 1")),
                ("C:/h/agentlife.exe", Ok("something else 1.0")),
            ],
        );
        assert_eq!(l[0].verdict, Verdict::Unreadable);
        assert!(l[0].detail.contains("exit code 1"));
        assert_eq!(l[1].verdict, Verdict::Unreadable);
    }

    #[test]
    fn a_wrong_program_at_the_path_is_not_taken_for_the_right_one() {
        let l = lines(
            &BOTH,
            &[
                ("C:/b/lane-restart.exe", Ok("agentlife 0.12.3")),
                ("C:/h/agentlife.exe", Ok("lane-restart 0.12.3")),
            ],
        );
        assert_eq!(l[0].verdict, Verdict::Unreadable);
        assert_eq!(l[1].verdict, Verdict::Unreadable);
    }

    #[test]
    fn the_rendering_says_it_changed_nothing() {
        let text = render(&lines(&[], &[]));
        assert!(text.contains("MISSING"));
        assert!(text.contains("Read-only: nothing was installed"));
    }
}
