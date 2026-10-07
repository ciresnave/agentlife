// SPDX-License-Identifier: MIT OR Apache-2.0
//! Is a running agent safe to stop? (DESIGN.md §3, DESIGN-REVISION-2 §3, DESIGN-REVISION-3 §4.2.)
//!
//! An interactive `claude` cannot be asked "are you busy" from outside (OverMind's finding, verified
//! against its docs), and its `busy` flag can be wrong in the unsafe direction (a lost `PreToolUse`
//! hook, see `.lane-state/hook-errors.log`). So no single signal is trusted, and **unknown counts as
//! unsafe**. An agent is ready to stop only when **all** of these hold, each checked against evidence
//! gathered *after* agentlife asked it to wrap up:
//!
//! 1. its **HANDOFF** was written after the request (the lane's own statement of where things stand);
//! 2. its `lane-restart` state record, **joined by session id**, never by role name, which is the
//!    phantom-file defect measured on 2026-10-03, says it **asserted idle after the request**
//!    (`no_background_shells: true`), is **not busy** and has **no subagents**;
//! 3. **no live shell** sits below its `claude` in the process tree (a backgrounded command is a real
//!    child process even though no hook fires for it).
//!
//! This module only *reads*: `.lane-state` is OverMind's runtime directory and agentlife never writes it.

use crate::hook::SHELLS;
use crate::registry::AgentRecord;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The few fields of a `lane-restart` state record this needs. Everything but the session id and
/// the time may be missing, and a missing field is **unsafe**, not assumed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LaneStateView {
    pub session_id: String,
    #[serde(default)]
    pub busy: Option<bool>,
    #[serde(default)]
    pub subagents_running: Option<u32>,
    #[serde(default)]
    pub no_background_shells: Option<bool>,
    pub updated_at: DateTime<Utc>,
    /// The hook event that last wrote the record (`SessionStart` until the session does anything).
    #[serde(default)]
    pub updated_by_event: Option<String>,
}

/// The newest state record in `dir` whose `session_id` is `session_id`. Files that do not parse are
/// skipped (another tool owns them); a missing directory is "no record".
pub fn read_lane_state(dir: &Path, session_id: &str) -> Option<LaneStateView> {
    let rd = std::fs::read_dir(dir).ok()?;
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<LaneStateView>(&text).ok())
        .filter(|v| v.session_id == session_id)
        .max_by_key(|v| v.updated_at)
}

/// Where an agent's HANDOFF may be: `<launch dir>/HANDOFF.md`, and `<launch dir>/<NAME>-HANDOFF.md`
/// (so the PM's `PM-HANDOFF.md` in the portfolio root is found). The name is used only if it is a
/// plain identifier, never an arbitrary string from a record.
pub fn handoff_candidates(rec: &AgentRecord) -> Vec<PathBuf> {
    let base = PathBuf::from(&rec.launch_cwd);
    let mut out = vec![base.join("HANDOFF.md")];
    for name in [rec.name.as_deref(), rec.role.as_deref()]
        .into_iter()
        .flatten()
    {
        let plain = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        let candidate = base.join(format!("{}-HANDOFF.md", name.to_ascii_uppercase()));
        if plain && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The most recently modified of `paths` that exists.
pub fn newest_mtime(paths: &[PathBuf]) -> Option<(PathBuf, DateTime<Utc>)> {
    paths
        .iter()
        .filter_map(|p| {
            let t = std::fs::metadata(p).ok()?.modified().ok()?;
            Some((p.clone(), DateTime::<Utc>::from(t)))
        })
        .max_by_key(|(_, t)| *t)
}

/// The shells running anywhere below a process.
pub trait ProcTree {
    /// `(pid, image name)` of every descendant of `pid`, lower-case without `.exe`.
    fn descendants(&self, pid: u32) -> Vec<(u32, String)>;
}

/// The shells among `pid`'s descendants.
pub fn shells_below(tree: &dyn ProcTree, pid: u32) -> Vec<(u32, String)> {
    tree.descendants(pid)
        .into_iter()
        .filter(|(_, name)| SHELLS.contains(&name.as_str()))
        .collect()
}

pub struct Evidence<'a> {
    pub state: Option<&'a LaneStateView>,
    /// The newest HANDOFF found, and when it was written.
    pub handoff: Option<&'a (PathBuf, DateTime<Utc>)>,
    pub handoff_looked_in: &'a [PathBuf],
    pub shells: &'a [(u32, String)],
    /// When agentlife asked the agent to wrap up.
    pub asked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    pub blockers: Vec<String>,
}

pub fn check(e: &Evidence) -> Readiness {
    let mut blockers = Vec::new();
    match e.handoff {
        None => blockers.push(format!(
            "no HANDOFF file found (looked for: {})",
            e.handoff_looked_in
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Some((path, at)) if *at <= e.asked_at => blockers.push(format!(
            "the HANDOFF {} was last written {}, before the request at {}",
            path.display(),
            at.format("%H:%M:%S"),
            e.asked_at.format("%H:%M:%S")
        )),
        Some(_) => {}
    }
    match e.state {
        None => blockers.push(
            "no lane-restart state record for this session (is its hook installed?), so idleness cannot be confirmed"
                .to_string(),
        ),
        Some(s) => {
            match s.busy {
                Some(false) => {}
                Some(true) => blockers.push("the lane is busy".to_string()),
                None => blockers.push("whether the lane is busy is unknown".to_string()),
            }
            match s.subagents_running {
                Some(0) => {}
                Some(n) => blockers.push(format!("{n} subagent(s) still running")),
                None => blockers.push("the number of running subagents is unknown".to_string()),
            }
            match s.no_background_shells {
                Some(true) if s.updated_at > e.asked_at => {}
                Some(true) => blockers.push(
                    "its idle claim (`lane-restart assert-idle`) predates the request".to_string(),
                ),
                _ => blockers.push(
                    "it has not asserted that it is idle (`lane-restart assert-idle`)".to_string(),
                ),
            }
        }
    }
    for (pid, name) in e.shells {
        blockers.push(format!(
            "a live shell is running below it: {name} (pid {pid})"
        ));
    }
    Readiness {
        ready: blockers.is_empty(),
        blockers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AgentId;
    use chrono::TimeZone;
    use std::collections::HashMap;

    fn t(m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 3, m, s).unwrap()
    }

    fn state(
        busy: Option<bool>,
        subs: Option<u32>,
        claim: Option<bool>,
        at: DateTime<Utc>,
    ) -> LaneStateView {
        LaneStateView {
            session_id: "s1".into(),
            busy,
            subagents_running: subs,
            no_background_shells: claim,
            updated_at: at,
            updated_by_event: None,
        }
    }

    fn handoff(at: DateTime<Utc>) -> (PathBuf, DateTime<Utc>) {
        (PathBuf::from("C:/x/HANDOFF.md"), at)
    }

    fn run(
        state: Option<&LaneStateView>,
        handoff: Option<&(PathBuf, DateTime<Utc>)>,
        shells: &[(u32, String)],
    ) -> Readiness {
        let looked = [PathBuf::from("C:/x/HANDOFF.md")];
        check(&Evidence {
            state,
            handoff,
            handoff_looked_in: &looked,
            shells,
            asked_at: t(10, 0),
        })
    }

    #[test]
    fn ready_only_when_every_condition_holds() {
        let s = state(Some(false), Some(0), Some(true), t(11, 0));
        let h = handoff(t(10, 30));
        let r = run(Some(&s), Some(&h), &[]);
        assert!(r.ready, "{:?}", r.blockers);
        assert!(r.blockers.is_empty());
    }

    /// One condition broken at a time, each against an otherwise-ready lane, asserted by NAME.
    #[test]
    fn each_condition_alone_blocks() {
        let good_state = state(Some(false), Some(0), Some(true), t(11, 0));
        let good_handoff = handoff(t(10, 30));
        let shell = [(77u32, "bash".to_string())];
        // (name, state, handoff, shells, the text the single blocker must contain)
        type Case<'a> = (
            &'a str,
            Option<LaneStateView>,
            Option<(PathBuf, DateTime<Utc>)>,
            &'a [(u32, String)],
            &'a str,
        );
        let cases: Vec<Case> = vec![
            (
                "no handoff",
                Some(good_state.clone()),
                None,
                &[],
                "no HANDOFF file found",
            ),
            (
                "stale handoff",
                Some(good_state.clone()),
                Some(handoff(t(9, 0))),
                &[],
                "before the request",
            ),
            (
                "handoff at the same instant",
                Some(good_state.clone()),
                Some(handoff(t(10, 0))),
                &[],
                "before the request",
            ),
            (
                "no state record",
                None,
                Some(good_handoff.clone()),
                &[],
                "no lane-restart state record",
            ),
            (
                "busy",
                Some(state(Some(true), Some(0), Some(true), t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "busy",
            ),
            (
                "busy unknown",
                Some(state(None, Some(0), Some(true), t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "busy is unknown",
            ),
            (
                "subagents running",
                Some(state(Some(false), Some(2), Some(true), t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "2 subagent",
            ),
            (
                "subagents unknown",
                Some(state(Some(false), None, Some(true), t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "subagents is unknown",
            ),
            (
                "never asserted idle",
                Some(state(Some(false), Some(0), None, t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "not asserted",
            ),
            (
                "asserted not idle",
                Some(state(Some(false), Some(0), Some(false), t(11, 0))),
                Some(good_handoff.clone()),
                &[],
                "not asserted",
            ),
            (
                "idle claim predates the request",
                Some(state(Some(false), Some(0), Some(true), t(9, 0))),
                Some(good_handoff.clone()),
                &[],
                "predates the request",
            ),
            (
                "a live shell below it",
                Some(good_state.clone()),
                Some(good_handoff.clone()),
                &shell,
                "live shell is running below it: bash (pid 77)",
            ),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (name, st, h, shells, expect) in cases {
            let r = run(st.as_ref(), h.as_ref(), shells);
            assert!(!r.ready, "{name} must block");
            assert_eq!(
                r.blockers.len(),
                1,
                "{name}: exactly one blocker, got {:?}",
                r.blockers
            );
            assert!(
                r.blockers[0].contains(expect),
                "{name}: {:?} should mention {expect:?}",
                r.blockers
            );
            assert!(seen.insert(name));
        }
        assert_eq!(seen.len(), 12);
    }

    #[test]
    fn every_blocker_is_reported_not_just_the_first() {
        let s = state(Some(true), Some(1), None, t(9, 0));
        let r = run(Some(&s), None, &[(1, "pwsh".into())]);
        assert!(r.blockers.len() >= 5, "{:?}", r.blockers);
        assert!(!r.ready);
    }

    struct Tree(HashMap<u32, Vec<(u32, String)>>);
    impl ProcTree for Tree {
        fn descendants(&self, pid: u32) -> Vec<(u32, String)> {
            self.0.get(&pid).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn only_shells_below_the_lane_block_not_other_children() {
        let tree = Tree(HashMap::from([(
            10,
            vec![
                (11, "bun".to_string()),
                (12, "node".to_string()),
                (13, "bash".to_string()),
                (14, "cmd".to_string()),
                (15, "pwsh".to_string()),
            ],
        )]));
        let shells = shells_below(&tree, 10);
        assert_eq!(
            shells.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(),
            ["bash", "cmd", "pwsh"],
            "the MCP helper (bun) and node are not shells"
        );
        assert!(shells_below(&tree, 99).is_empty());
    }

    #[test]
    fn the_state_record_is_found_by_session_id_not_by_its_file_name() {
        let d = tempfile::tempdir().unwrap();
        let write = |name: &str, session: &str, at: &str, busy: bool| {
            std::fs::write(
                d.path().join(name),
                serde_json::json!({"session_id": session, "busy": busy, "subagents_running": 0,
                    "no_background_shells": true, "updated_at": at, "role": "whatever", "pid": 1})
                .to_string(),
            )
            .unwrap();
        };
        // The phantom-file defect: this lane's record sits under a worktree's name, and the file
        // named for the lane belongs to a different session.
        write("some-worktree.json", "s1", "2026-10-07T03:11:00Z", false);
        write(
            "synapse.json",
            "other-session",
            "2026-10-07T03:50:00Z",
            true,
        );
        write("older-copy.json", "s1", "2026-10-07T03:01:00Z", true);
        std::fs::write(d.path().join("broken.json"), "{ not json").unwrap();
        std::fs::write(d.path().join("synapse.lock"), "x").unwrap();
        let got = read_lane_state(d.path(), "s1").unwrap();
        assert_eq!(got.busy, Some(false), "the NEWEST record of that session");
        assert_eq!(
            got.updated_at,
            Utc.with_ymd_and_hms(2026, 10, 7, 3, 11, 0).unwrap()
        );
        assert!(read_lane_state(d.path(), "nobody").is_none());
        assert!(read_lane_state(&d.path().join("missing"), "s1").is_none());
    }

    #[test]
    fn a_record_missing_optional_fields_is_unsafe_not_ready() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("x.json"),
            r#"{"session_id":"s1","updated_at":"2026-10-07T03:11:00Z"}"#,
        )
        .unwrap();
        let s = read_lane_state(d.path(), "s1").unwrap();
        assert_eq!(
            (s.busy, s.subagents_running, s.no_background_shells),
            (None, None, None)
        );
        let h = handoff(t(10, 30));
        assert!(!run(Some(&s), Some(&h), &[]).ready);
    }

    #[test]
    fn handoff_candidates_use_the_launch_dir_and_only_plain_names() {
        let mut r = AgentRecord::new(AgentId::new("a1").unwrap(), "C:/Projects", t(0, 0));
        r.name = Some("PM".into());
        r.role = Some("pm".into());
        let c = handoff_candidates(&r);
        assert_eq!(
            c,
            [
                PathBuf::from("C:/Projects/HANDOFF.md"),
                PathBuf::from("C:/Projects/PM-HANDOFF.md")
            ],
            "name and role give the same file once"
        );
        r.name = Some("../evil;calc".into());
        r.role = None;
        assert_eq!(
            handoff_candidates(&r),
            [PathBuf::from("C:/Projects/HANDOFF.md")]
        );
    }

    #[test]
    fn the_newest_existing_handoff_wins_and_a_missing_one_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("HANDOFF.md");
        let b = d.path().join("PM-HANDOFF.md");
        std::fs::write(&a, "old").unwrap();
        let old = std::fs::OpenOptions::new().write(true).open(&a).unwrap();
        old.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        drop(old);
        std::fs::write(&b, "new").unwrap();
        let missing = d.path().join("NOPE-HANDOFF.md");
        let (p, _) = newest_mtime(&[a.clone(), missing.clone(), b.clone()]).unwrap();
        assert_eq!(p, b);
        assert!(newest_mtime(&[missing]).is_none());
    }
}
