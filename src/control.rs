// SPDX-License-Identifier: MIT OR Apache-2.0
//! `park`, `stop` and `unpark --no-start` for an agent that is **not running** (M2a).
//!
//! CireSnave: *"a user or project manager agent should be able to park a lane or otherwise stop a
//! lane ... Lanes should be parkable so that they are still known about and can be launched again
//! quickly but are not always required to be running."* This module is the registry half of that:
//! it marks an agent closed (with who, how and when), or clears the mark. It **never touches a
//! process**. A running agent is refused (`Running`), because stopping one gracefully is its own
//! change (M2b): it must ask the lane to write its HANDOFF first (DESIGN.md §3).
//!
//! `unpark` without `--no-start` would *launch* the agent, which needs the launcher (M3) and consent
//! (M4); only the registry half, `--no-start`, exists here.
//!
//! **Who may** (DESIGN-REVISION-2 §4): a person, any time; the PM agent, on an agent that is not
//! running; any other agent, nothing, because stopping someone else's lane needs a person's consent
//! (M4). A **pinned** agent (the PM, or an explicit pin) additionally needs the caller to type its
//! name with `--confirm` (R3-Q3: an explicit park of the PM needs an extra typed confirmation).

use crate::caller::Caller;
use crate::config::Config;
use crate::identity::ProcessTable;
use crate::journal::Journal;
use crate::list::{liveness, Liveness};
use crate::marks::{pin_kind, PinKind};
use crate::registry::{AgentId, AgentRecord, ClosedHow, Intent, Registry};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Park,
    Stop,
    Unpark,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlError {
    NotFound(AgentId),
    Denied(String),
    /// The agent's process is alive. Stopping it gracefully is M2b.
    Running {
        pid: Option<u32>,
    },
    AlreadyClosed,
    NotClosed,
    /// A pinned agent: type its name with `--confirm`.
    NeedsConfirmation {
        expected: String,
    },
    Registry(String),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlError::NotFound(id) => write!(f, "no agent {id}"),
            ControlError::Denied(why) => write!(f, "refused: {why}"),
            ControlError::Running { pid } => write!(
                f,
                "the agent is running{}; stopping a running agent gracefully (it writes its HANDOFF first) is not built yet, so nothing was changed",
                pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
            ),
            ControlError::AlreadyClosed => write!(f, "the agent is already closed"),
            ControlError::NotClosed => write!(f, "the agent is not closed"),
            ControlError::NeedsConfirmation { expected } => write!(
                f,
                "this agent is pinned; type its name to confirm: --confirm {expected}"
            ),
            ControlError::Registry(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ControlError {}

fn is_pm(rec: Option<&AgentRecord>, cfg: &Config) -> bool {
    rec.is_some_and(|r| pin_kind(r, cfg) == Some(PinKind::Rule))
}

/// The label recorded in `Intent::Closed { by }` and in the journal.
pub fn by_label(caller: &Caller, caller_rec: Option<&AgentRecord>, cfg: &Config) -> String {
    match caller {
        Caller::Person => "person".to_string(),
        Caller::Agent { id: Some(id) } if is_pm(caller_rec, cfg) => format!("pm-agent:{id}"),
        Caller::Agent { id: Some(id) } => format!("agent:{id}"),
        Caller::Agent { id: None } => "agent:unregistered".to_string(),
        Caller::Unclear(_) => "unclear".to_string(),
    }
}

/// May `caller` take `action` on `target`? Policy only; the agent's state is checked separately.
pub fn authorize(
    caller: &Caller,
    caller_rec: Option<&AgentRecord>,
    target: &AgentRecord,
    action: Action,
    cfg: &Config,
) -> Result<(), String> {
    let verb = match action {
        Action::Park => "park",
        Action::Stop => "stop",
        Action::Unpark => "unpark",
    };
    match caller {
        Caller::Person => Ok(()),
        Caller::Agent { id: Some(id) } if id == &target.agent_id && action != Action::Unpark => Ok(()),
        Caller::Agent { id: Some(_) } if is_pm(caller_rec, cfg) => Ok(()),
        Caller::Agent { .. } => Err(format!(
            "an agent may {verb} only itself (the PM, any agent that is not running); {verb}ing another agent needs a person's consent, which is not built yet"
        )),
        Caller::Unclear(why) => Err(format!("cannot tell who is calling ({why}); treated as untrusted")),
    }
}

fn load(registry: &Registry, id: &AgentId) -> Result<AgentRecord, ControlError> {
    registry
        .get(id)
        .map_err(|e| ControlError::Registry(e.to_string()))?
        .ok_or_else(|| ControlError::NotFound(id.clone()))
}

/// Marks a **stopped** agent closed. `how` is `Parked` (kept forever, listed prominently) or
/// `Exited` (ages out), the two labels of DESIGN-REVISION-2 §2.
#[allow(clippy::too_many_arguments)]
pub fn close(
    registry: &Registry,
    journal: &Journal,
    table: &dyn ProcessTable,
    cfg: &Config,
    caller: &Caller,
    caller_rec: Option<&AgentRecord>,
    id: &AgentId,
    how: ClosedHow,
    confirm: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), ControlError> {
    let rec = load(registry, id)?;
    let action = if how == ClosedHow::Parked {
        Action::Park
    } else {
        Action::Stop
    };
    authorize(caller, caller_rec, &rec, action, cfg).map_err(ControlError::Denied)?;
    if matches!(rec.intent, Intent::Closed { .. }) {
        return Err(ControlError::AlreadyClosed);
    }
    let (live, pid) = liveness(&rec, table);
    if live != Liveness::Stopped {
        return Err(ControlError::Running { pid });
    }
    if pin_kind(&rec, cfg).is_some() {
        let expected = rec.name.clone().unwrap_or_else(|| rec.agent_id.to_string());
        if !confirm.is_some_and(|c| c.eq_ignore_ascii_case(&expected)) {
            return Err(ControlError::NeedsConfirmation { expected });
        }
    }
    let by = by_label(caller, caller_rec, cfg);
    let mut applied = false;
    registry
        .update(id, |r| {
            if !matches!(r.intent, Intent::Closed { .. }) {
                r.intent = Intent::Closed {
                    how,
                    by: by.clone(),
                    at: now,
                };
                applied = true;
            }
        })
        .map_err(|e| ControlError::Registry(e.to_string()))?;
    if !applied {
        return Err(ControlError::AlreadyClosed);
    }
    let _ = journal.append(
        "closed",
        Some(id.as_str()),
        json!({"how": how, "by": by, "pinned": pin_kind(&rec, cfg).is_some()}),
    );
    Ok(())
}

/// `unpark --no-start`: clears the closed mark so the agent is a restore candidate again. Starts
/// nothing.
pub fn unpark_no_start(
    registry: &Registry,
    journal: &Journal,
    cfg: &Config,
    caller: &Caller,
    caller_rec: Option<&AgentRecord>,
    id: &AgentId,
) -> Result<(), ControlError> {
    let rec = load(registry, id)?;
    authorize(caller, caller_rec, &rec, Action::Unpark, cfg).map_err(ControlError::Denied)?;
    if !matches!(rec.intent, Intent::Closed { .. }) {
        return Err(ControlError::NotClosed);
    }
    let by = by_label(caller, caller_rec, cfg);
    let mut applied = false;
    registry
        .update(id, |r| {
            if matches!(r.intent, Intent::Closed { .. }) {
                r.intent = Intent::Wanted;
                applied = true;
            }
        })
        .map_err(|e| ControlError::Registry(e.to_string()))?;
    if !applied {
        return Err(ControlError::NotClosed);
    }
    let _ = journal.append(
        "reopened",
        Some(id.as_str()),
        json!({"by": by, "start": false}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::identity::ProcessIdentity;
    use crate::registry::Session;
    use chrono::TimeZone;
    use std::sync::Arc;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, h, 0, 0).unwrap()
    }
    fn id(s: &str) -> AgentId {
        AgentId::new(s).unwrap()
    }

    struct Alive(Vec<(u32, u64)>);
    impl ProcessTable for Alive {
        fn identity_of(&self, pid: u32) -> Option<ProcessIdentity> {
            self.0
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|(p, s)| ProcessIdentity {
                    pid: *p,
                    start_secs: *s,
                    exe: None,
                })
        }
    }

    fn agent(
        i: &str,
        name: &str,
        cwd: &str,
        role: Option<&str>,
        running_as: Option<(u32, u64)>,
    ) -> AgentRecord {
        let mut r = AgentRecord::new(id(i), cwd, t(1));
        r.name = Some(name.to_string());
        r.role = role.map(String::from);
        if let Some((pid, start)) = running_as {
            r.sessions.push(Session {
                session_id: format!("s-{i}"),
                pid,
                process_start_secs: Some(start),
                started_at: t(1),
                ended_at: None,
                end_reason: None,
            });
        }
        r
    }

    struct Rig {
        _d: tempfile::TempDir,
        registry: Registry,
        journal: Journal,
    }
    fn rig(records: &[AgentRecord]) -> Rig {
        let d = tempfile::tempdir().unwrap();
        let rig = Rig {
            registry: Registry::new(d.path().join("agents")),
            journal: Journal::new(d.path().join("journal"), Arc::new(SystemClock)),
            _d: d,
        };
        for r in records {
            rig.registry.create(r).unwrap();
        }
        rig
    }

    fn closed(
        r: &Rig,
        i: &str,
        how: ClosedHow,
        caller: &Caller,
        caller_rec: Option<&AgentRecord>,
        confirm: Option<&str>,
        table: &Alive,
    ) -> Result<(), ControlError> {
        close(
            &r.registry,
            &r.journal,
            table,
            &Config::default(),
            caller,
            caller_rec,
            &id(i),
            how,
            confirm,
            t(5),
        )
    }

    #[test]
    fn a_person_parks_a_stopped_agent_and_it_is_recorded_with_who_how_and_when() {
        let r = rig(&[agent("a1", "lane", "C:/Projects/lane", None, None)]);
        closed(
            &r,
            "a1",
            ClosedHow::Parked,
            &Caller::Person,
            None,
            None,
            &Alive(vec![]),
        )
        .unwrap();
        assert_eq!(
            r.registry.get(&id("a1")).unwrap().unwrap().intent,
            Intent::Closed {
                how: ClosedHow::Parked,
                by: "person".into(),
                at: t(5)
            }
        );
        let e = r.journal.read_all().unwrap().entries;
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, "closed");
        assert_eq!(e[0].data["how"], "parked");
        assert_eq!(e[0].data["by"], "person");
    }

    #[test]
    fn stop_marks_exited_and_a_second_close_is_refused() {
        let r = rig(&[agent("a1", "lane", "C:/Projects/lane", None, None)]);
        closed(
            &r,
            "a1",
            ClosedHow::Exited,
            &Caller::Person,
            None,
            None,
            &Alive(vec![]),
        )
        .unwrap();
        assert!(matches!(
            r.registry.get(&id("a1")).unwrap().unwrap().intent,
            Intent::Closed {
                how: ClosedHow::Exited,
                ..
            }
        ));
        assert_eq!(
            closed(
                &r,
                "a1",
                ClosedHow::Parked,
                &Caller::Person,
                None,
                None,
                &Alive(vec![])
            ),
            Err(ControlError::AlreadyClosed)
        );
        assert!(
            matches!(
                r.registry.get(&id("a1")).unwrap().unwrap().intent,
                Intent::Closed {
                    how: ClosedHow::Exited,
                    ..
                }
            ),
            "the refused second close changed nothing"
        );
    }

    #[test]
    fn a_running_agent_is_refused_and_left_untouched() {
        let r = rig(&[agent(
            "a1",
            "lane",
            "C:/Projects/lane",
            None,
            Some((100, 5000)),
        )]);
        let table = Alive(vec![(100, 5000)]);
        assert_eq!(
            closed(
                &r,
                "a1",
                ClosedHow::Parked,
                &Caller::Person,
                None,
                None,
                &table
            ),
            Err(ControlError::Running { pid: Some(100) })
        );
        assert_eq!(
            r.registry.get(&id("a1")).unwrap().unwrap().intent,
            Intent::Wanted
        );
        assert!(r.journal.read_all().unwrap().entries.is_empty());
        // Positive control: the same agent, once its process is gone, can be parked.
        assert!(closed(
            &r,
            "a1",
            ClosedHow::Parked,
            &Caller::Person,
            None,
            None,
            &Alive(vec![])
        )
        .is_ok());
    }

    #[test]
    fn who_may_park_whom() {
        let pm = agent("pm-id", "PM", "C:/Projects", Some("pm"), Some((1, 1)));
        let lane = agent(
            "lane-id",
            "synapse",
            "C:/Projects/synapse",
            None,
            Some((2, 2)),
        );
        let target = agent("t-id", "target", "C:/Projects/t", None, None);
        let r = rig(&[pm.clone(), lane.clone(), target]);
        let table = Alive(vec![(1, 1), (2, 2)]);
        // Another lane cannot park it, and nothing changes.
        let other = Caller::Agent {
            id: Some(id("lane-id")),
        };
        assert!(matches!(
            closed(
                &r,
                "t-id",
                ClosedHow::Parked,
                &other,
                Some(&lane),
                None,
                &table
            ),
            Err(ControlError::Denied(_))
        ));
        assert_eq!(
            r.registry.get(&id("t-id")).unwrap().unwrap().intent,
            Intent::Wanted
        );
        // The PM agent can, and is recorded as the PM.
        let pm_caller = Caller::Agent {
            id: Some(id("pm-id")),
        };
        closed(
            &r,
            "t-id",
            ClosedHow::Parked,
            &pm_caller,
            Some(&pm),
            None,
            &table,
        )
        .unwrap();
        assert_eq!(
            r.registry.get(&id("t-id")).unwrap().unwrap().intent,
            Intent::Closed {
                how: ClosedHow::Parked,
                by: "pm-agent:pm-id".into(),
                at: t(5)
            }
        );
        // An unregistered or unclear caller is refused.
        assert!(authorize(
            &Caller::Agent { id: None },
            None,
            &lane,
            Action::Park,
            &Config::default()
        )
        .is_err());
        assert!(authorize(
            &Caller::Unclear("x".into()),
            None,
            &lane,
            Action::Park,
            &Config::default()
        )
        .is_err());
    }

    #[test]
    fn a_pinned_agent_needs_its_name_typed_to_be_parked() {
        // R3-Q3: parking the PM needs an extra typed confirmation. The same holds for any pin.
        let pm = agent("pm-id", "PM", "C:/Projects", Some("pm"), None);
        let mut pinned = agent("p-id", "pinned-lane", "C:/Projects/p", None, None);
        pinned.pinned = Some(crate::registry::Pin {
            by: "person".into(),
            at: t(1),
        });
        let r = rig(&[pm, pinned]);
        let dead = Alive(vec![]);
        for (i, name) in [("pm-id", "PM"), ("p-id", "pinned-lane")] {
            assert_eq!(
                closed(&r, i, ClosedHow::Parked, &Caller::Person, None, None, &dead),
                Err(ControlError::NeedsConfirmation {
                    expected: name.into()
                }),
                "{name}: no confirmation"
            );
            assert!(
                matches!(
                    closed(
                        &r,
                        i,
                        ClosedHow::Parked,
                        &Caller::Person,
                        None,
                        Some("wrong"),
                        &dead
                    ),
                    Err(ControlError::NeedsConfirmation { .. })
                ),
                "{name}: wrong confirmation"
            );
            assert_eq!(
                r.registry.get(&id(i)).unwrap().unwrap().intent,
                Intent::Wanted
            );
            closed(
                &r,
                i,
                ClosedHow::Parked,
                &Caller::Person,
                None,
                Some(&name.to_lowercase()),
                &dead,
            )
            .unwrap_or_else(|e| panic!("{name}: the right name, any case, should confirm: {e}"));
        }
    }

    #[test]
    fn unpark_no_start_clears_the_mark_and_starts_nothing() {
        let r = rig(&[agent("a1", "lane", "C:/Projects/lane", None, None)]);
        let cfg = Config::default();
        assert_eq!(
            unpark_no_start(
                &r.registry,
                &r.journal,
                &cfg,
                &Caller::Person,
                None,
                &id("a1")
            ),
            Err(ControlError::NotClosed),
            "an agent that is not closed cannot be reopened"
        );
        closed(
            &r,
            "a1",
            ClosedHow::Parked,
            &Caller::Person,
            None,
            None,
            &Alive(vec![]),
        )
        .unwrap();
        unpark_no_start(
            &r.registry,
            &r.journal,
            &cfg,
            &Caller::Person,
            None,
            &id("a1"),
        )
        .unwrap();
        let rec = r.registry.get(&id("a1")).unwrap().unwrap();
        assert_eq!(rec.intent, Intent::Wanted);
        assert!(rec.sessions.is_empty(), "nothing was started");
        let kinds: Vec<_> = r
            .journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["closed", "reopened"]);
    }

    #[test]
    fn an_agent_cannot_unpark_even_itself_only_the_pm_or_a_person() {
        let lane = agent("lane-id", "lane", "C:/Projects/lane", None, None);
        let r = rig(std::slice::from_ref(&lane));
        closed(
            &r,
            "lane-id",
            ClosedHow::Parked,
            &Caller::Person,
            None,
            None,
            &Alive(vec![]),
        )
        .unwrap();
        let me = Caller::Agent {
            id: Some(id("lane-id")),
        };
        assert!(matches!(
            unpark_no_start(
                &r.registry,
                &r.journal,
                &Config::default(),
                &me,
                Some(&lane),
                &id("lane-id")
            ),
            Err(ControlError::Denied(_))
        ));
        assert!(matches!(
            r.registry.get(&id("lane-id")).unwrap().unwrap().intent,
            Intent::Closed { .. }
        ));
    }

    #[test]
    fn an_unknown_agent_is_not_found() {
        let r = rig(&[]);
        assert_eq!(
            closed(
                &r,
                "ghost",
                ClosedHow::Parked,
                &Caller::Person,
                None,
                None,
                &Alive(vec![])
            ),
            Err(ControlError::NotFound(id("ghost")))
        );
    }
}
