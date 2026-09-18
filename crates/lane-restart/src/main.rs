// SPDX-License-Identifier: MIT OR Apache-2.0

//! Restarts a Claude Code lane session with the same identity.
//!
//! Design: `RESTART-TOOL-DESIGN.md` at the repo root. Settled, not yet
//! implemented — this is the crate skeleton (workspace, CI, arg shape) built
//! ahead of the kill/launch logic, which needs the design's four decisions
//! folded in first (done) and its own review before it touches a live
//! process. Nothing here sends a signal to anything yet.

use std::process::ExitCode;

#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// Restart the lane invoking this tool - the process's own parent,
    /// identified from this process's own environment, not from an argument
    /// a caller could spoof.
    Myself,
    /// Restart a different, named lane. Only ever eligible when that lane's
    /// own state file says idle - see RESTART-TOOL-DESIGN.md §3.
    Role(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    target: Target,
    dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ArgError {
    MissingRole,
    Unknown(String),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::MissingRole => write!(f, "--role requires a value"),
            ArgError::Unknown(a) => write!(f, "unrecognised argument: {a}"),
        }
    }
}

/// Parses argv (excluding the program name). ⚠️ `--yes` is accepted here as
/// a flag shape only - RESTART-TOOL-DESIGN.md §6.2 makes clear it does not,
/// by itself, authorize anything: a restart of another lane still refuses
/// unless that lane's own state file says idle, checked fresh at kill time.
fn parse_args<I: IntoIterator<Item = String>>(argv: I) -> Result<Args, ArgError> {
    let mut target = Target::Myself;
    let mut dry_run = false;
    let mut iter = argv.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--yes" => {} // accepted; authorization still comes from the idle check, not this flag
            "--role" => {
                let role = iter.next().ok_or(ArgError::MissingRole)?;
                target = Target::Role(role);
            }
            other => return Err(ArgError::Unknown(other.to_string())),
        }
    }
    Ok(Args { target, dry_run })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(argv) {
        Ok(args) => {
            eprintln!(
                "lane-restart: not yet implemented (target={:?}, dry_run={}). \
                 See RESTART-TOOL-DESIGN.md.",
                args.target, args.dry_run
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("lane-restart: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_to_restarting_self_not_dry_run() {
        let parsed = parse_args(args(&[])).unwrap();
        assert_eq!(parsed.target, Target::Myself);
        assert!(!parsed.dry_run);
    }

    #[test]
    fn role_selects_a_named_lane() {
        let parsed = parse_args(args(&["--role", "synapse"])).unwrap();
        assert_eq!(parsed.target, Target::Role("synapse".to_string()));
    }

    #[test]
    fn dry_run_is_recognised() {
        let parsed = parse_args(args(&["--dry-run"])).unwrap();
        assert!(parsed.dry_run);
    }

    #[test]
    fn role_without_a_value_is_a_clear_error() {
        assert_eq!(
            parse_args(args(&["--role"])).unwrap_err(),
            ArgError::MissingRole
        );
    }

    #[test]
    fn an_unknown_flag_is_refused_not_ignored() {
        assert_eq!(
            parse_args(args(&["--frobnicate"])).unwrap_err(),
            ArgError::Unknown("--frobnicate".to_string())
        );
    }
}
