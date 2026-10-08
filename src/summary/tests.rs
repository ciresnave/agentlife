use super::*;
use crate::plan::{Excluded, Held, Params, Reason, PLAN_SCHEMA};

fn entry(id: &str, mode: Option<&str>) -> Entry {
    Entry {
        agent_id: id.into(),
        name: Some(id.into()),
        title: id.into(),
        cwd: format!("C:/Projects/{id}"),
        argv: vec![],
        mode: mode.map(Into::into),
        pm: false,
        flags: vec![],
        dropped_args: vec![],
        batch: 1,
        window: 1,
        tab: 1,
    }
}

fn plan(entries: Vec<Entry>) -> Plan {
    let mut p = Plan {
        schema: PLAN_SCHEMA,
        params: Params {
            batch_size: 3,
            batch_delay_secs: 30,
            liveness_timeout_secs: 60,
            tabs_per_window: 4,
            max_running: None,
            free_ram_floor_gb: 4.0,
        },
        running_now: 0,
        free_ram_gb: None,
        entries,
        held: vec![],
        excluded: vec![],
        hash: String::new(),
    };
    p.hash = p.compute_hash();
    p
}

#[test]
fn exceptions_come_before_the_list_and_routine_exclusions_are_not_refusals() {
    let old = plan(vec![entry("a", None), entry("b", Some("default"))]);
    let mut new = plan(vec![
        entry("a", Some("bypassPermissions")),
        entry("b", Some("default")),
        entry("c", None),
    ]);
    new.entries[1].flags.push("dropped channel".into());
    new.excluded.push(Excluded {
        agent_id: "x".into(),
        name: Some("x".into()),
        reason: Reason::BypassNotPm,
        detail: "refused".into(),
    });
    new.excluded.push(Excluded {
        agent_id: "y".into(),
        name: Some("yroutine".into()),
        reason: Reason::NotWanted,
        detail: String::new(),
    });
    new.held.push(Held {
        agent_id: "h".into(),
        name: Some("h".into()),
        why: "memory".into(),
    });
    let s = render(&new, Some(&old), 10, "p1");
    let at = |needle: &str| {
        s.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing in\n{s}"))
    };
    assert!(at("refused by a hard rule") < at("mode wider"));
    assert!(at("mode wider") < at("flagged"));
    assert!(at("flagged") < at("new since"));
    assert!(at("new since") < at("held back"));
    assert!(
        s[at("new since")..at("held back")].contains(
            "  c
"
        ),
        "the new agent is listed:
{s}"
    );
    assert!(at("held back") < at("would start"));
    assert!(s.contains("a: default -> bypassPermissions"));
    assert!(s.contains("b: dropped channel"));
    assert!(s.contains("3 agents to start, 1 held, 1 refused"), "{s}");
    assert!(!s.contains("yroutine") && !s.contains("routine"), "{s}");
}

#[test]
fn a_long_list_is_capped_and_says_how_many_more() {
    let p = plan(
        (0..250)
            .map(|i| entry(&format!("agent{i:03}"), None))
            .collect(),
    );
    let s = render(&p, None, 10, "p1");
    assert!(s.contains("would start (250):"));
    assert!(s.contains("... and 240 more"));
    assert!(s.contains("agent009") && !s.contains("agent010"));
    assert!(s.lines().count() < 20, "bounded:\n{s}");
}

#[test]
fn without_an_earlier_approved_plan_it_says_new_and_widened_cannot_be_told() {
    let s = render(&plan(vec![entry("a", None)]), None, 10, "p1");
    assert!(s.contains("no usable earlier approved restore"));
    assert!(!s.contains("new since"));
}

#[test]
fn narrower_or_equal_modes_are_not_widened_and_an_unknown_mode_is() {
    let old = plan(vec![
        entry("a", Some("auto")),
        entry("b", Some("auto")),
        entry("c", None),
    ]);
    let new = plan(vec![
        entry("a", Some("default")),
        entry("b", Some("auto")),
        entry("c", Some("mystery")),
    ]);
    let s = render(&new, Some(&old), 10, "p1");
    assert!(
        s.contains("mode wider than at the last approved restore (1):"),
        "{s}"
    );
    assert!(s.contains("c: default -> mystery"));
}

#[test]
fn control_characters_in_recorded_values_cannot_forge_lines() {
    let mut e = entry(
        "evil
refused by a hard rule (0):[2J",
        None,
    );
    e.flags.push(
        "x
y"
        .into(),
    );
    let s = render(&plan(vec![e]), None, 10, "p1");
    assert!(!s.contains(''), "{s:?}");
    assert!(
        !s.contains(
            "
refused by a hard rule (0)"
        ),
        "{s:?}"
    );
    assert!(!s.contains(
        "x
y"
    ));
}
