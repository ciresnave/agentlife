// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart wakes [--json]` - lists the wake-at registry (`wakes.rs`, `docs/WAKE-AT.md`): every
//! pending wake with its time, the time the launcher acts, and its state. Read-only.

use super::ctx::Ctx;
use crate::spec::{self, Lifecycle};
use crate::wakes::{self, Registry};
use std::process::ExitCode;

pub(super) fn run(ctx: &mut Ctx, json: bool) -> ExitCode {
    let state = ctx.state_dir.clone();
    let reg = match Registry::load(&state) {
        Ok(r) => r,
        Err(e) => {
            ctx.errln(&format!("lane-restart wakes: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let report = spec::status_all(&state, &*ctx.facts());
    let running = |role: &str| {
        report
            .roles
            .iter()
            .any(|r| r.role == role && r.state == Lifecycle::Running && r.activity.is_some())
    };
    let now = lane_state::timestamp::Timestamp::now();
    let rows = wakes::readout(&reg, now, &running);
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "role": r.role, "event": r.event, "wake": r.wake_utc.to_rfc3339(),
                    "due": r.due_utc.to_rfc3339(), "state": r.state,
                })
            })
            .collect();
        ctx.outln(&serde_json::Value::Array(v).to_string());
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() {
        ctx.outln("no wakes registered");
        return ExitCode::SUCCESS;
    }
    for r in &rows {
        let left = r.due_utc.secs_since(now);
        let left = if left <= 0 {
            "due".to_string()
        } else {
            format!("in {}m", left / 60)
        };
        ctx.outln(&format!(
            "{}	{}	wake {}	due {} ({})	{}",
            r.role,
            r.state,
            r.wake_utc.format("%Y-%m-%dT%H:%MZ"),
            r.due_utc.format("%Y-%m-%dT%H:%M:%SZ"),
            left,
            r.event
        ));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::run_with;
    use std::ffi::OsString;

    fn args(rest: &[&str]) -> Vec<OsString> {
        std::iter::once("lane-restart")
            .chain(rest.iter().copied())
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn wakes_lists_a_registered_wake_and_says_so_when_none() {
        let d = tempfile::tempdir().unwrap();
        let mut ctx = Ctx::capture(d.path().join("state"), Ok(d.path().to_path_buf()), None);
        assert_eq!(
            format!("{:?}", run_with(&mut ctx, args(&["wakes"]))),
            format!("{:?}", ExitCode::SUCCESS)
        );
        assert_eq!(
            ctx.captured().0,
            "no wakes registered
"
        );

        wakes::register_from_handoff(
            &d.path().join("state"),
            "r",
            "WAIT: build | check: true | wake: 2099-01-01T00:00:00Z
",
            lane_state::timestamp::Timestamp::now(),
        )
        .unwrap();
        let mut ctx = Ctx::capture(d.path().join("state"), Ok(d.path().to_path_buf()), None);
        let _ = run_with(&mut ctx, args(&["wakes"]));
        let (out, _) = ctx.captured();
        assert!(out.starts_with("r	pending	wake 2099-01-01T00:00Z"), "{out}");
        assert!(out.contains("	build"), "{out}");

        let mut ctx = Ctx::capture(d.path().join("state"), Ok(d.path().to_path_buf()), None);
        let _ = run_with(&mut ctx, args(&["wakes", "--json"]));
        assert!(ctx.captured().0.contains("\"state\":\"pending\""));
    }
}
