// SPDX-License-Identifier: MIT OR Apache-2.0

//! The decision: may THIS restart happen. RESTART-TOOL-DESIGN.md §2, §3, §6.2.
//!
//! ⚠️ THIS IS THE DANGEROUS PART. Every refusal path here is deliberately its
//! own, separately-tested branch - not a single catch-all boolean - because a
//! catch-all is exactly the shape of bug that turns "refuse" into "proceed"
//! when one condition is mis-negated. `decide` never kills or launches
//! anything; it only says whether the caller may, and in which mode.

use crate::facts::{ProcessIdentity, SystemFacts};
use crate::state::{self, LaneState};
use std::path::Path;
use std::time::Duration;

/// §2.4: a state file older than this is not trusted - the lane may have
/// crashed without `SessionEnd` ever firing to update or remove it.
pub const STALE_STATE_AFTER: Duration = Duration::from_secs(120);

/// §2.3: how old a matching transcript may be and still count as "this
/// process's own", guarding against a reused PID's leftover state file
/// pointing at a genuinely old transcript. Generous on purpose - a real,
/// still-open session can sit idle for hours without writing.
pub const TRANSCRIPT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The lane restarting itself. RESTART-TOOL-DESIGN.md §3: no idle check -
    /// the requester IS the process being restarted, so there is no
    /// staleness window between "checked" and "acted."
    Myself { role: String },
    /// A different lane, restarted only if it is independently confirmed
    /// idle - RESTART-TOOL-DESIGN.md §3.
    Other { role: String },
}

impl Target {
    fn role(&self) -> &str {
        match self {
            Target::Myself { role } | Target::Other { role } => role,
        }
    }

    fn is_other(&self) -> bool {
        matches!(self, Target::Other { .. })
    }
}

#[derive(Debug)]
pub struct Request {
    pub target: Target,
    /// `--yes`. Only meaningful for `Target::Other` - RESTART-TOOL-DESIGN.md §6.2.
    pub confirmed: bool,
    /// `--dry-run`, explicit. `Target::Other` without `--yes` is ALSO
    /// effectively dry-run regardless of this flag - see `Plan::will_act`.
    pub dry_run: bool,
}

#[derive(Debug, PartialEq)]
pub enum Refusal {
    NoStateFile(String),
    MalformedStateFile(String),
    StaleStateFile { role: String, age_secs: i64 },
    ProcessNotIdentified(String),
    Busy(String),
    SubagentsRunning(String, u32),
    NoBackgroundShellsClaimMissing(String),
    LiveShellDetected(String),
    ShellCheckFailed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoStateFile(r) => write!(f, "{r}: no state file"),
            Refusal::MalformedStateFile(r) => write!(f, "{r}: state file is not valid"),
            Refusal::StaleStateFile { role, age_secs } => {
                write!(
                    f,
                    "{role}: state file is {age_secs}s old, refusing as stale"
                )
            }
            Refusal::ProcessNotIdentified(msg) => write!(f, "identity check failed: {msg}"),
            Refusal::Busy(r) => write!(f, "{r}: busy"),
            Refusal::SubagentsRunning(r, n) => write!(f, "{r}: {n} subagent(s) still running"),
            Refusal::NoBackgroundShellsClaimMissing(r) => {
                write!(
                    f,
                    "{r}: no_background_shells was never asserted (or is false)"
                )
            }
            Refusal::LiveShellDetected(r) => {
                write!(
                    f,
                    "{r}: a live shell descendant was found on the process tree"
                )
            }
            Refusal::ShellCheckFailed(msg) => {
                write!(
                    f,
                    "shell check could not run cleanly, refusing as unsafe: {msg}"
                )
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub state: LaneState,
    /// Recorded at decision time, re-checked immediately before the real
    /// kill signal - PM finding, 2026-09-18: a PID alone can be recycled in
    /// the window between deciding and acting.
    pub identity: ProcessIdentity,
    /// The restart tool must ACT (kill + relaunch) rather than only print,
    /// exactly when this is true.
    pub will_act: bool,
}

fn identify(state: &LaneState, facts: &dyn SystemFacts) -> Result<ProcessIdentity, Refusal> {
    let age = facts.now().signed_duration_since(state.updated_at);
    let age_secs = age.num_seconds();
    if age_secs < 0 || age_secs as u64 > STALE_STATE_AFTER.as_secs() {
        return Err(Refusal::StaleStateFile {
            role: state.role.clone(),
            age_secs,
        });
    }

    if !facts.is_alive_claude_process(state.pid) {
        return Err(Refusal::ProcessNotIdentified(format!(
            "{}: pid {} is not a live claude process",
            state.role, state.pid
        )));
    }

    match facts.cwd_of(state.pid) {
        // ⚠️ PM finding, 2026-09-18 (real restart attempt): a real Windows
        // process's own cwd carries a trailing separator the hook's
        // recorded cwd never has - `crate::paths::paths_match` normalises
        // both sides (separators, a trailing one, case) rather than a raw
        // `==`, without ever treating one path as a PREFIX of another.
        Some(cwd) if crate::paths::paths_match(&cwd.to_string_lossy(), &state.cwd) => {}
        Some(cwd) => {
            return Err(Refusal::ProcessNotIdentified(format!(
                "{}: pid {} is in {cwd:?}, state file says {:?}",
                state.role, state.pid, state.cwd
            )))
        }
        None => {
            return Err(Refusal::ProcessNotIdentified(format!(
                "{}: could not read the cwd of pid {}",
                state.role, state.pid
            )))
        }
    }

    if !facts.transcript_is_recent(&state.cwd, &state.session_id, TRANSCRIPT_MAX_AGE) {
        return Err(Refusal::ProcessNotIdentified(format!(
            "{}: no transcript for session {} is recent enough",
            state.role, state.session_id
        )));
    }

    facts.process_identity(state.pid).ok_or_else(|| {
        Refusal::ProcessNotIdentified(format!(
            "{}: could not read a process identity for pid {} to record",
            state.role, state.pid
        ))
    })
}

/// §1a: BOTH signals, refuse if either shows activity or can't be checked.
fn idle_and_shell_free(state: &LaneState, facts: &dyn SystemFacts) -> Result<(), Refusal> {
    if state.busy {
        return Err(Refusal::Busy(state.role.clone()));
    }
    if state.subagents_running > 0 {
        return Err(Refusal::SubagentsRunning(
            state.role.clone(),
            state.subagents_running,
        ));
    }
    if state.no_background_shells != Some(true) {
        return Err(Refusal::NoBackgroundShellsClaimMissing(state.role.clone()));
    }
    match facts.has_live_shell_descendant(state.pid) {
        Ok(true) => Err(Refusal::LiveShellDetected(state.role.clone())),
        Ok(false) => Ok(()),
        Err(e) => Err(Refusal::ShellCheckFailed(format!("{}: {e:?}", state.role))),
    }
}

/// The one entry point. Loads the target's state file, runs the identity
/// check (always), and - only for `Target::Other` - the idle+shell check.
/// Never touches a real process; only decides.
pub fn decide(req: &Request, facts: &dyn SystemFacts, state_dir: &Path) -> Result<Plan, Refusal> {
    let role = req.target.role();
    let state = state::load(state_dir, role).map_err(|e| match e {
        state::LoadError::NotFound(_) => Refusal::NoStateFile(role.to_string()),
        state::LoadError::Unreadable(m) | state::LoadError::Malformed(m) => {
            Refusal::MalformedStateFile(format!("{role}: {m}"))
        }
    })?;

    let identity = identify(&state, facts)?;

    if req.target.is_other() {
        idle_and_shell_free(&state, facts)?;
    }

    let will_act = match req.target {
        Target::Myself { .. } => !req.dry_run,
        Target::Other { .. } => !req.dry_run && req.confirmed,
    };

    Ok(Plan {
        state,
        identity,
        will_act,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::ShellCheckError;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A fully-controllable fake. Every method's answer is set per test, so
    /// a test that doesn't set a given fact fails loudly rather than
    /// silently falling back to something that looks safe.
    struct FakeFacts {
        now: chrono::DateTime<Utc>,
        alive_claude: HashMap<u32, bool>,
        cwds: HashMap<u32, PathBuf>,
        shells: HashMap<u32, Result<bool, ShellCheckError>>,
        transcripts_recent: RefCell<bool>,
        /// Returned by `process_identity` at decide()-time, and again by
        /// `kill_verified` - a test that wants to simulate PID reuse
        /// between those two calls uses `RefCell` to change this between
        /// its own two calls into the fake.
        identities: RefCell<HashMap<u32, ProcessIdentity>>,
    }

    impl FakeFacts {
        fn new() -> Self {
            Self {
                now: Utc::now(),
                alive_claude: HashMap::new(),
                cwds: HashMap::new(),
                shells: HashMap::new(),
                transcripts_recent: RefCell::new(true),
                identities: RefCell::new(HashMap::new()),
            }
        }
        fn alive(mut self, pid: u32) -> Self {
            self.alive_claude.insert(pid, true);
            self.identities.borrow_mut().insert(
                pid,
                ProcessIdentity {
                    start_time_secs: 1000,
                    exe: Some(PathBuf::from("claude.exe")),
                },
            );
            self
        }
        fn dead(mut self, pid: u32) -> Self {
            self.alive_claude.insert(pid, false);
            self
        }
        fn cwd(mut self, pid: u32, cwd: &str) -> Self {
            self.cwds.insert(pid, PathBuf::from(cwd));
            self
        }
        fn no_shell(mut self, pid: u32) -> Self {
            self.shells.insert(pid, Ok(false));
            self
        }
        fn has_shell(mut self, pid: u32) -> Self {
            self.shells.insert(pid, Ok(true));
            self
        }
        fn shell_check_broken(mut self, pid: u32) -> Self {
            self.shells
                .insert(pid, Err(ShellCheckError::EnumerationFailed("boom".into())));
            self
        }
        fn transcript_stale(self) -> Self {
            *self.transcripts_recent.borrow_mut() = false;
            self
        }
    }

    impl SystemFacts for FakeFacts {
        fn is_alive_claude_process(&self, pid: u32) -> bool {
            *self.alive_claude.get(&pid).unwrap_or(&false)
        }
        fn cwd_of(&self, pid: u32) -> Option<PathBuf> {
            self.cwds.get(&pid).cloned()
        }
        fn has_live_shell_descendant(&self, pid: u32) -> Result<bool, ShellCheckError> {
            self.shells.get(&pid).cloned().unwrap_or(Ok(false))
        }
        fn transcript_is_recent(&self, _cwd: &str, _session_id: &str, _max_age: Duration) -> bool {
            *self.transcripts_recent.borrow()
        }
        fn now(&self) -> chrono::DateTime<Utc> {
            self.now
        }
        fn process_identity(&self, pid: u32) -> Option<ProcessIdentity> {
            self.identities.borrow().get(&pid).cloned()
        }
        fn kill_verified(
            &self,
            _pid: u32,
            _expected: &ProcessIdentity,
        ) -> Result<(), crate::facts::KillError> {
            panic!("decide() must never call kill_verified() - it only decides")
        }
        fn find_claude_process_in(&self, _cwd: &str, _after_start_time_secs: u64) -> Option<u32> {
            panic!("decide() must never call find_claude_process_in() - it only decides")
        }
    }

    const PID: u32 = 4200;
    const CWD: &str = "C:/Projects/OverMind";

    fn write_state(dir: &Path, role: &str, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut v = serde_json::json!({
            "role": role,
            "session_id": "3f9a1111-2222-3333-4444-555555555555",
            "pid": PID,
            "cwd": CWD,
            "name": role,
            "model": "claude-sonnet-5",
            "permission_mode": "prompting",
            "remote_control": true,
            "busy": false,
            "subagents_running": 0,
            "no_background_shells": true,
            "updated_at": Utc::now().to_rfc3339(),
            "updated_by_event": "Stop"
        });
        edit(&mut v);
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
    }

    fn healthy_facts() -> FakeFacts {
        FakeFacts::new().alive(PID).cwd(PID, CWD).no_shell(PID)
    }

    // --- self-restart: no idle/shell check should ever run --------------- //

    #[test]
    fn self_restart_succeeds_even_if_busy() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |v| {
            v["busy"] = serde_json::json!(true);
        });
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        let plan = decide(&req, &healthy_facts(), dir.path()).unwrap();
        assert!(plan.will_act, "self-restart never needs --yes");
    }

    #[test]
    fn self_restart_with_dry_run_does_not_act() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: true,
        };
        let plan = decide(&req, &healthy_facts(), dir.path()).unwrap();
        assert!(!plan.will_act);
    }

    #[test]
    fn self_restart_still_requires_identity_to_match() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = FakeFacts::new().dead(PID); // identity fails: pid not alive
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ProcessNotIdentified(_))
        ));
    }

    // --- identity check, each of its parts -------------------------------- //

    #[test]
    fn refuses_a_dead_pid() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = FakeFacts::new().dead(PID).cwd(PID, CWD).no_shell(PID);
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ProcessNotIdentified(_))
        ));
    }

    #[test]
    fn refuses_a_cwd_mismatch() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = FakeFacts::new()
            .alive(PID)
            .cwd(PID, "C:/Somewhere/Else")
            .no_shell(PID);
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ProcessNotIdentified(_))
        ));
    }

    /// ⚠️ PM finding, 2026-09-18 (real restart attempt, second retest): a
    /// real Windows process's own cwd carries a trailing separator the
    /// hook's recorded cwd never has - the identity check must accept that,
    /// not refuse over formatting. `state.cwd` here (`CWD`, no trailing
    /// separator, the form a real hook payload arrives in) is exactly what
    /// the state file carries; only the FACT the real process reports has
    /// the trailing separator, the same shape a live Windows `cwd_of` read
    /// actually produces.
    #[test]
    fn accepts_a_cwd_with_a_trailing_separator_the_state_file_does_not_have() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = FakeFacts::new()
            .alive(PID)
            .cwd(PID, "C:/Projects/OverMind/")
            .no_shell(PID);
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(decide(&req, &facts, dir.path()).is_ok());
    }

    #[test]
    fn refuses_when_cwd_cannot_be_read() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = FakeFacts::new().alive(PID).no_shell(PID); // no cwd set -> None
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ProcessNotIdentified(_))
        ));
    }

    #[test]
    fn refuses_a_stale_transcript() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let facts = healthy_facts().transcript_stale();
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ProcessNotIdentified(_))
        ));
    }

    #[test]
    fn refuses_a_stale_state_file() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |v| {
            let old = Utc::now() - ChronoDuration::seconds(300);
            v["updated_at"] = serde_json::json!(old.to_rfc3339());
        });
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::StaleStateFile { .. })
        ));
    }

    #[test]
    fn refuses_a_future_dated_state_file_too() {
        // ⚠️ Not just "old" - a clock skew or a crafted file dated in the
        // future must not read as "very fresh."
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |v| {
            let future = Utc::now() + ChronoDuration::seconds(300);
            v["updated_at"] = serde_json::json!(future.to_rfc3339());
        });
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::StaleStateFile { .. })
        ));
    }

    #[test]
    fn refuses_a_missing_state_file() {
        let dir = tempdir().unwrap();
        let req = Request {
            target: Target::Myself {
                role: "nonexistent".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::NoStateFile(_))
        ));
    }

    // --- restarting a DIFFERENT lane: idle + shell checks apply ----------- //

    #[test]
    fn refuses_another_lane_that_is_busy() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |v| {
            v["busy"] = serde_json::json!(true);
        });
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert_eq!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::Busy("synapse".into()))
        );
    }

    #[test]
    fn refuses_another_lane_with_subagents_running() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |v| {
            v["subagents_running"] = serde_json::json!(2);
        });
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert_eq!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::SubagentsRunning("synapse".into(), 2))
        );
    }

    #[test]
    fn refuses_when_no_background_shells_was_never_asserted() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |v| {
            v.as_object_mut().unwrap().remove("no_background_shells");
        });
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert_eq!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::NoBackgroundShellsClaimMissing("synapse".into()))
        );
    }

    #[test]
    fn refuses_when_no_background_shells_was_explicitly_false() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |v| {
            v["no_background_shells"] = serde_json::json!(false);
        });
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert_eq!(
            decide(&req, &healthy_facts(), dir.path()),
            Err(Refusal::NoBackgroundShellsClaimMissing("synapse".into()))
        );
    }

    #[test]
    fn refuses_when_the_process_tree_shows_a_live_shell() {
        // The claim says no shells, but the independent check disagrees -
        // §1a: EITHER signal showing activity refuses.
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |_| {});
        let facts = FakeFacts::new().alive(PID).cwd(PID, CWD).has_shell(PID);
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert_eq!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::LiveShellDetected("synapse".into()))
        );
    }

    #[test]
    fn refuses_when_the_shell_check_itself_fails() {
        // ⚠️ Unknown must count as unsafe, not as "no shell found."
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |_| {});
        let facts = FakeFacts::new()
            .alive(PID)
            .cwd(PID, CWD)
            .shell_check_broken(PID);
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        assert!(matches!(
            decide(&req, &facts, dir.path()),
            Err(Refusal::ShellCheckFailed(_))
        ));
    }

    #[test]
    fn another_lane_without_yes_is_downgraded_to_dry_run_not_refused() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |_| {});
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: false, // no --yes
            dry_run: false,
        };
        let plan = decide(&req, &healthy_facts(), dir.path()).unwrap();
        assert!(
            !plan.will_act,
            "missing --yes must silently downgrade to dry-run, not error"
        );
    }

    #[test]
    fn another_lane_with_yes_and_idle_and_fresh_acts_for_real() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |_| {});
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true,
            dry_run: false,
        };
        let plan = decide(&req, &healthy_facts(), dir.path()).unwrap();
        assert!(plan.will_act);
    }

    #[test]
    fn the_plan_carries_the_process_identity_recorded_at_decide_time() {
        // ⚠️ main.rs's kill step relies on THIS identity, not a fresh read
        // of its own - decide() must actually capture and hand it back.
        let dir = tempdir().unwrap();
        write_state(dir.path(), "overmind", |_| {});
        let req = Request {
            target: Target::Myself {
                role: "overmind".into(),
            },
            confirmed: false,
            dry_run: false,
        };
        let plan = decide(&req, &healthy_facts(), dir.path()).unwrap();
        assert_eq!(plan.identity.start_time_secs, 1000);
        assert_eq!(plan.identity.exe, Some(PathBuf::from("claude.exe")));
    }

    #[test]
    fn yes_never_overrides_a_busy_refusal() {
        let dir = tempdir().unwrap();
        write_state(dir.path(), "synapse", |v| {
            v["busy"] = serde_json::json!(true);
        });
        let req = Request {
            target: Target::Other {
                role: "synapse".into(),
            },
            confirmed: true, // --yes present
            dry_run: false,
        };
        assert!(
            decide(&req, &healthy_facts(), dir.path()).is_err(),
            "--yes must never be able to override a real busy/shell refusal"
        );
    }
}
