// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pin and "waiting on the user" marks (DESIGN-REVISION-3 §5).
//!
//! CireSnave: the PM *"should never be"* shut down into a load-on-demand state, and the same holds
//! for *"any agent that is actively waiting for an answer from the user in their own chat."* Two of
//! the signals are here, the ones that need no new hook:
//!
//! * **S1, a pin.** Definitive. The PM is pinned by a visible rule, never a hidden default: an agent
//!   launched in `portfolio_root` whose role or name is in `pin_roles`. Anything else is pinned only
//!   by an explicit `pin`.
//! * **S2, a waiting mark.** Written by the agent about itself (`agentlife waiting --on user`), with
//!   a time-to-live so a forgotten mark cannot pin an agent forever.
//!
//! **Who may do it** (DESIGN-REVISION-3 §15, R3-Q6 and DESIGN-REVISION-2 §4): an agent may mark
//! **itself**; the PM agent or a person may mark another. An *explicit* pin does **not** make an
//! agent "the PM" for authorization: any lane may pin itself, so counting that would let any lane
//! promote itself. Only the **rule** identifies the PM, and the rule keys on fields the agent
//! reports about itself (role, launch directory), which is the same-user limit this project states
//! everywhere, not a boundary.

use crate::caller::Caller;
use crate::config::Config;
use crate::identity::paths_equal;
use crate::journal::Journal;
use crate::registry::{AgentId, AgentRecord, Pin, Registry, WaitingMark};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKind {
    /// `agentlife pin` was run for it.
    Explicit,
    /// The `pin_roles` + `portfolio_root` rule names it (the PM).
    Rule,
}

pub fn pin_kind(rec: &AgentRecord, cfg: &Config) -> Option<PinKind> {
    if rec.pinned.is_some() {
        return Some(PinKind::Explicit);
    }
    let listed = |s: &str| cfg.pin_roles.iter().any(|r| r.eq_ignore_ascii_case(s));
    let named = rec.role.as_deref().is_some_and(listed) || rec.name.as_deref().is_some_and(listed);
    (named && paths_equal(&rec.launch_cwd, &cfg.portfolio_root)).then_some(PinKind::Rule)
}

/// Whether the mark is still in force: present and younger than the time-to-live.
pub fn is_waiting(rec: &AgentRecord, now: DateTime<Utc>, ttl_hours: u32) -> bool {
    rec.waiting
        .as_ref()
        .is_some_and(|w| now - w.since < Duration::hours(i64::from(ttl_hours)))
}

/// May `caller` mark `target`? `caller_rec` is the caller's own record when it is an agent.
pub fn authorize(
    caller: &Caller,
    caller_rec: Option<&AgentRecord>,
    target: &AgentId,
    cfg: &Config,
) -> Result<(), String> {
    match caller {
        Caller::Person => Ok(()),
        Caller::Agent { id: Some(id) } if id == target => Ok(()),
        Caller::Agent { id: Some(_) } => {
            if caller_rec.is_some_and(|r| pin_kind(r, cfg) == Some(PinKind::Rule)) {
                Ok(())
            } else {
                Err(
                    "an agent may mark only itself; marking another needs the PM or a person"
                        .into(),
                )
            }
        }
        Caller::Agent { id: None } => Err(
            "this session is not registered as an agent, so it cannot be identified as itself"
                .into(),
        ),
        Caller::Unclear(why) => Err(format!(
            "cannot tell who is calling ({why}); treated as untrusted"
        )),
    }
}

fn who(caller: &Caller) -> String {
    match caller {
        Caller::Person => "person".to_string(),
        Caller::Agent { id: Some(id) } => format!("agent:{id}"),
        Caller::Agent { id: None } => "agent:unregistered".to_string(),
        Caller::Unclear(_) => "unclear".to_string(),
    }
}

pub fn pin(
    registry: &Registry,
    journal: &Journal,
    id: &AgentId,
    caller: &Caller,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let by = who(caller);
    registry
        .update(id, |r| {
            r.pinned = Some(Pin {
                by: by.clone(),
                at: now,
            });
        })
        .map_err(|e| e.to_string())?;
    let _ = journal.append("pinned", Some(id.as_str()), json!({"by": by}));
    Ok(())
}

pub fn unpin(
    registry: &Registry,
    journal: &Journal,
    id: &AgentId,
    caller: &Caller,
) -> Result<(), String> {
    let by = who(caller);
    registry
        .update(id, |r| r.pinned = None)
        .map_err(|e| e.to_string())?;
    let _ = journal.append("unpinned", Some(id.as_str()), json!({"by": by}));
    Ok(())
}

pub fn set_waiting(
    registry: &Registry,
    journal: &Journal,
    id: &AgentId,
    note: Option<String>,
    caller: &Caller,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let by = who(caller);
    registry
        .update(id, |r| {
            r.waiting = Some(WaitingMark {
                on: "user".to_string(),
                since: now,
                note: note.clone(),
            });
        })
        .map_err(|e| e.to_string())?;
    let _ = journal.append(
        "waiting-set",
        Some(id.as_str()),
        json!({"by": by, "on": "user"}),
    );
    Ok(())
}

pub fn clear_waiting(
    registry: &Registry,
    journal: &Journal,
    id: &AgentId,
    caller: &Caller,
) -> Result<(), String> {
    let by = who(caller);
    registry
        .update(id, |r| r.waiting = None)
        .map_err(|e| e.to_string())?;
    let _ = journal.append("waiting-cleared", Some(id.as_str()), json!({"by": by}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use chrono::TimeZone;
    use std::sync::Arc;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, h, m, 0).unwrap()
    }

    fn rec(id: &str, name: &str, role: Option<&str>, cwd: &str) -> AgentRecord {
        let mut r = AgentRecord::new(AgentId::new(id).unwrap(), cwd, t(1, 0));
        r.name = Some(name.to_string());
        r.role = role.map(String::from);
        r
    }

    fn id(s: &str) -> AgentId {
        AgentId::new(s).unwrap()
    }

    #[test]
    fn the_pm_is_pinned_by_a_visible_rule_not_by_a_hidden_default() {
        let cfg = Config::default(); // pin_roles ["pm"], portfolio_root C:/Projects
                                     // By role, in the portfolio root.
        assert_eq!(
            pin_kind(&rec("a", "x", Some("pm"), "C:/Projects"), &cfg),
            Some(PinKind::Rule)
        );
        // By name, in the portfolio root, any case, either separator.
        assert_eq!(
            pin_kind(&rec("b", "PM", None, "C:\\Projects\\"), &cfg),
            Some(PinKind::Rule)
        );
        // The right name in the wrong directory is NOT the PM.
        assert_eq!(
            pin_kind(&rec("c", "PM", Some("pm"), "C:/Projects/fuel"), &cfg),
            None
        );
        // The right directory with another name is not the PM either.
        assert_eq!(
            pin_kind(&rec("d", "scratch", None, "C:/Projects"), &cfg),
            None
        );
        // A pinned-by-hand agent is Explicit, and the rule does not need to apply.
        let mut e = rec("e", "lane", None, "C:/Projects/lane");
        e.pinned = Some(Pin {
            by: "person".into(),
            at: t(2, 0),
        });
        assert_eq!(pin_kind(&e, &cfg), Some(PinKind::Explicit));
    }

    #[test]
    fn the_rule_follows_the_configuration() {
        let cfg = Config {
            pin_roles: vec!["lead".to_string()],
            portfolio_root: "D:/ws".to_string(),
            ..Config::default()
        };
        assert_eq!(
            pin_kind(&rec("a", "Lead", None, "D:/ws"), &cfg),
            Some(PinKind::Rule)
        );
        assert_eq!(
            pin_kind(&rec("b", "pm", Some("pm"), "C:/Projects"), &cfg),
            None
        );
    }

    #[test]
    fn a_waiting_mark_expires_after_its_time_to_live() {
        let mut r = rec("a", "x", None, "C:/x");
        assert!(!is_waiting(&r, t(5, 0), 24));
        r.waiting = Some(WaitingMark {
            on: "user".into(),
            since: t(1, 0),
            note: None,
        });
        assert!(is_waiting(&r, t(1, 30), 1), "30 minutes into a 1 hour ttl");
        assert!(
            !is_waiting(&r, t(2, 0), 1),
            "at exactly the ttl it has expired"
        );
        assert!(!is_waiting(&r, t(3, 0), 1));
        assert!(is_waiting(&r, t(23, 0), 24));
    }

    #[test]
    fn who_may_mark_whom() {
        let cfg = Config::default();
        let pm = rec("pm-id", "PM", Some("pm"), "C:/Projects");
        let lane = rec("lane-id", "synapse", None, "C:/Projects/synapse");
        let other = id("other-id");
        let me = Caller::Agent {
            id: Some(id("lane-id")),
        };
        let pm_caller = Caller::Agent {
            id: Some(id("pm-id")),
        };

        assert!(
            authorize(&Caller::Person, None, &other, &cfg).is_ok(),
            "a person: anything"
        );
        assert!(
            authorize(&me, Some(&lane), &id("lane-id"), &cfg).is_ok(),
            "an agent: itself"
        );
        assert!(
            authorize(&me, Some(&lane), &other, &cfg).is_err(),
            "an agent: not another"
        );
        assert!(
            authorize(&pm_caller, Some(&pm), &other, &cfg).is_ok(),
            "the PM: another"
        );
        assert!(authorize(&Caller::Agent { id: None }, None, &other, &cfg).is_err());
        assert!(authorize(&Caller::Unclear("why".into()), None, &other, &cfg).is_err());
    }

    #[test]
    fn a_lane_that_pins_itself_does_not_become_the_pm() {
        // The escalation this guards: pinning yourself is allowed (R3-Q6), so if an EXPLICIT pin
        // counted as "the PM" for authorization, any lane could promote itself in one command.
        let cfg = Config::default();
        let mut sneaky = rec("sneaky", "scratch", None, "C:/Projects/scratch");
        sneaky.pinned = Some(Pin {
            by: "agent:sneaky".into(),
            at: t(1, 0),
        });
        let caller = Caller::Agent {
            id: Some(id("sneaky")),
        };
        assert_eq!(pin_kind(&sneaky, &cfg), Some(PinKind::Explicit));
        assert!(
            authorize(&caller, Some(&sneaky), &id("victim"), &cfg).is_err(),
            "an explicit pin is not the PM"
        );
    }

    struct Rig {
        _d: tempfile::TempDir,
        registry: Registry,
        journal: Journal,
    }

    fn rig_with(r: &AgentRecord) -> Rig {
        let d = tempfile::tempdir().unwrap();
        let rig = Rig {
            registry: Registry::new(d.path().join("agents")),
            journal: Journal::new(d.path().join("journal"), Arc::new(SystemClock)),
            _d: d,
        };
        rig.registry.create(r).unwrap();
        rig
    }

    #[test]
    fn pin_and_unpin_write_the_record_and_the_journal() {
        let r = rec("a1", "lane", None, "C:/x");
        let rig = rig_with(&r);
        pin(
            &rig.registry,
            &rig.journal,
            &id("a1"),
            &Caller::Person,
            t(2, 0),
        )
        .unwrap();
        let got = rig.registry.get(&id("a1")).unwrap().unwrap();
        assert_eq!(
            got.pinned,
            Some(Pin {
                by: "person".into(),
                at: t(2, 0)
            })
        );
        unpin(&rig.registry, &rig.journal, &id("a1"), &Caller::Person).unwrap();
        assert_eq!(rig.registry.get(&id("a1")).unwrap().unwrap().pinned, None);
        let kinds: Vec<_> = rig
            .journal
            .read_all()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["pinned", "unpinned"]);
    }

    #[test]
    fn waiting_is_set_with_a_note_and_cleared() {
        let r = rec("a1", "lane", None, "C:/x");
        let rig = rig_with(&r);
        let me = Caller::Agent { id: Some(id("a1")) };
        set_waiting(
            &rig.registry,
            &rig.journal,
            &id("a1"),
            Some("needs your OK".into()),
            &me,
            t(3, 0),
        )
        .unwrap();
        let w = rig
            .registry
            .get(&id("a1"))
            .unwrap()
            .unwrap()
            .waiting
            .unwrap();
        assert_eq!(
            (w.on.as_str(), w.since, w.note.as_deref()),
            ("user", t(3, 0), Some("needs your OK"))
        );
        clear_waiting(&rig.registry, &rig.journal, &id("a1"), &me).unwrap();
        assert!(rig
            .registry
            .get(&id("a1"))
            .unwrap()
            .unwrap()
            .waiting
            .is_none());
        let e = rig.journal.read_all().unwrap().entries;
        assert_eq!(e[0].kind, "waiting-set");
        assert_eq!(e[0].data["by"], "agent:a1");
        assert_eq!(e[1].kind, "waiting-cleared");
    }

    #[test]
    fn marking_an_unknown_agent_is_an_error_not_a_creation() {
        let rig = rig_with(&rec("a1", "lane", None, "C:/x"));
        assert!(pin(
            &rig.registry,
            &rig.journal,
            &id("ghost"),
            &Caller::Person,
            t(2, 0)
        )
        .is_err());
        assert!(rig.registry.get(&id("ghost")).unwrap().is_none());
    }
}
