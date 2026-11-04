// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-provision` through the `Ctx` seam: roots in a tempdir, a fake verified caller, a host that
//! provisions nothing, and the `restart.log` rows read back. The machine's state directory, Synapse
//! and ledger are never touched.

use super::*;
use crate::app::run_with;
use crate::lane_new::Roots;
use crate::lane_new_host::{Host, ProvisionCounts};
use crate::start::CallerError;
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

struct NoHost;

impl Host for NoHost {
    fn disk_floor(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn provision(&self, _: &Path) -> Result<ProvisionCounts, String> {
        Err("a dry run must not provision".to_string())
    }
    fn start(&self, _: &str, _: &str) -> Result<i32, String> {
        Err("lane-provision never starts".to_string())
    }
    fn claude_alive(&self, _: u32) -> bool {
        false
    }
}

fn ok(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

fn failed(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::FAILURE)
}

fn args(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("lane-restart")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

fn roots(base: &Path) -> Roots {
    std::fs::create_dir_all(base.join("state")).unwrap();
    Roots::new(
        base.join("state"),
        base.join("proj"),
        base.join("homes"),
        base.join("pilot"),
        base.join("bin").join("chan"),
    )
    .unwrap()
}

fn ctx(roots: &Roots, caller: Option<&'static str>) -> Ctx {
    let make: Box<dyn Fn() -> Result<String, CallerError>> = match caller {
        Some(role) => Box::new(move || Ok(role.to_string())),
        None => Box::new(|| Err(CallerError::NoClaudeAncestor)),
    };
    let state = roots.state_dir.clone();
    Ctx::capture(state.clone(), Ok(state), None)
        .with_lane_new(Ok(roots.clone()), None, Some(Rc::new(NoHost)))
        .with_caller(make)
}

fn lane_spec(r: &Roots, role: &str) {
    let l = crate::lane_new::build_spec(r, role, "x", "sonnet", chrono::Utc::now()).unwrap();
    crate::spec::write_atomic(&r.state_dir, &l).unwrap();
}

fn rows(state: &Path) -> Vec<crate::ledger::Row> {
    crate::ledger::read(&state.join("restart.log"))
        .map(|l| l.rows)
        .unwrap_or_default()
}

#[test]
fn a_non_pm_caller_is_refused_by_name_and_recorded_and_nothing_is_written() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    lane_spec(&r, "bitman");
    for caller in [Some("alpha"), None] {
        let mut c = ctx(&r, caller);
        assert!(failed(run_with(
            &mut c,
            args(&["lane-provision", "bitman"])
        )));
        let (out, err) = c.captured();
        assert_eq!(out, "", "{caller:?}");
        assert!(
            err.starts_with("lane-restart lane-provision: refused - only the PM session"),
            "{err}"
        );
    }
    assert!(!r.state_dir.join("bitman.mcp.json").exists());
    let l = rows(&r.state_dir);
    assert_eq!(l.len(), 2);
    assert!(l
        .iter()
        .all(|x| !x.acted && x.kind.as_deref() == Some("lane-provision")));
}

#[test]
fn the_pm_dry_run_prints_the_plan_and_writes_nothing_and_a_bad_apply_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    lane_spec(&r, "bitman");
    let mut c = ctx(&r, Some("pm"));
    assert!(ok(run_with(&mut c, args(&["lane-provision", "bitman"]))));
    let (out, _) = c.captured();
    assert!(out.starts_with("lane-provision: DRY RUN"), "{out}");
    let plan = out
        .lines()
        .find_map(|l| l.strip_prefix("plan "))
        .expect("a plan hash")
        .to_string();
    assert!(
        out.contains(&format!("repeat the same role with: --apply --plan {plan}")),
        "{out}"
    );
    assert!(!r.state_dir.join("bitman.mcp.json").exists());
    assert!(rows(&r.state_dir).is_empty(), "a dry run logs nothing");
    // apply with a wrong plan: refused, recorded, nothing written
    let mut c = ctx(&r, Some("pm"));
    assert!(failed(run_with(
        &mut c,
        args(&[
            "lane-provision",
            "bitman",
            "--apply",
            "--plan",
            "0000000000000000"
        ])
    )));
    assert!(!r.state_dir.join("bitman.mcp.json").exists());
    let l = rows(&r.state_dir);
    assert_eq!(l.len(), 1);
    assert!(!l[0].acted);
}

#[test]
fn argument_errors_name_the_command() {
    let d = tempfile::tempdir().unwrap();
    let r = roots(d.path());
    for bad in [
        &["lane-provision"][..],
        &["lane-provision", "a", "--bogus"][..],
    ] {
        let mut c = ctx(&r, Some("pm"));
        assert!(failed(run_with(&mut c, args(bad))), "{bad:?}");
        let (out, err) = c.captured();
        assert_eq!(out, "", "{bad:?}");
        assert!(err.starts_with("lane-restart lane-provision: "), "{err}");
    }
}
