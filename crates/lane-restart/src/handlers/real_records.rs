//! The four Synapse approval records as they stand in `ciresnave/ciresnave`
//! (`.overmind/lane-restart/approvals/`), vendored under `tests/fixtures/approvals/` byte for byte
//! from commit 3b5d50823e57bec8316c670930b6a38d934074ec (2026-10-22T14:26:00Z). The other tests
//! build a record from the inert example with the Channels value swapped; none of them would
//! notice a typo, a wrong role or an expiry already past in the file a lane actually loads.

use super::tests::{REAL_DIALOG_SCREEN, TWO_SERVER_SCREEN};
use super::{is_active, matches, parse_and_validate, HandlerSpec};

const PILOT: &str = include_str!("../../tests/fixtures/approvals/synapse-dev-channel-pilot.json");
const BOTH_PILOT: &str =
    include_str!("../../tests/fixtures/approvals/claude-peers-synapse-dev-channels-pilot.json");
const RESTARTTEST: &str =
    include_str!("../../tests/fixtures/approvals/synapse-dev-channel-restarttest.json");
const PEERS: &str = include_str!("../../tests/fixtures/approvals/claude-peers-dev-channels.json");

const PEERS_ONLY: &str = "server:claude-peers";
const SYNAPSE_ONLY: &str = "server:synapse";
const BOTH: &str = "server:claude-peers, server:synapse";

/// (file text, the Channels value it must answer, roles in scope, last instant it is active).
/// Written out by hand from the records' own text, so a changed record fails here.
struct Expect {
    json: &'static str,
    channels: &'static str,
    roles: &'static [&'static str],
    expires: &'static str,
}

const RECORDS: [Expect; 4] = [
    Expect {
        json: PILOT,
        channels: SYNAPSE_ONLY,
        roles: &["synapse", "agentlife", "overmind"],
        expires: "2026-10-31T00:00:00Z",
    },
    Expect {
        json: BOTH_PILOT,
        channels: BOTH,
        roles: &["synapse", "agentlife", "overmind"],
        expires: "2026-10-31T00:00:00Z",
    },
    Expect {
        json: RESTARTTEST,
        channels: SYNAPSE_ONLY,
        roles: &["restarttest"],
        expires: "2026-11-09T00:00:00Z",
    },
    Expect {
        json: PEERS,
        channels: PEERS_ONLY,
        roles: &["*"],
        expires: "2027-03-19T00:00:00Z",
    },
];

fn handler(e: &Expect) -> HandlerSpec {
    parse_and_validate(e.json).expect("a record in the approvals repo must pass validation")
}

/// The real two-server capture with only its Channels value replaced. The real captures are
/// `REAL_DIALOG_SCREEN` (peers only) and `TWO_SERVER_SCREEN`; no real one-server synapse capture
/// exists yet (a built screen stands in until the first relaunch that stops at it keeps one).
fn screen_with(channels: &str) -> String {
    let text = TWO_SERVER_SCREEN.replace(BOTH, channels);
    assert!(
        text.contains(&format!("Channels: {channels}\n")),
        "anchor applied"
    );
    text
}

fn instant(s: &str) -> chrono::DateTime<chrono::Utc> {
    s.parse().unwrap()
}

#[test]
fn every_real_record_parses_and_says_what_this_test_expects() {
    for e in &RECORDS {
        let h = handler(e);
        assert_eq!(h.action, "1\r", "press 1 then Enter, no trailing newline");
        assert_eq!(h.scope.roles, e.roles, "roles in {}", e.channels);
        assert_eq!(h.expires_at, Some(instant(e.expires)));
    }
}

#[test]
fn every_real_record_is_active_for_its_roles_until_expiry_and_for_no_other_role() {
    for e in &RECORDS {
        let h = handler(e);
        let before = instant(e.expires) - chrono::Duration::seconds(1);
        for role in e.roles {
            let role = if *role == "*" { "any-lane" } else { role };
            assert!(is_active(&h, role, before), "{role} before expiry");
            assert!(!is_active(&h, role, instant(e.expires)), "{role} at expiry");
        }
        let outsider = is_active(&h, "not-a-lane-in-any-scope", before);
        assert_eq!(outsider, e.roles == ["*"], "outside role vs {:?}", e.roles);
    }
}

#[test]
fn every_real_record_answers_exactly_its_channel_set() {
    let sets = [PEERS_ONLY, SYNAPSE_ONLY, BOTH];
    for e in &RECORDS {
        let h = handler(e);
        for set in sets {
            assert_eq!(
                matches(&h, &screen_with(set)),
                set == e.channels,
                "record for {:?} vs screen {set:?}",
                e.channels
            );
        }
        // Not a prefix, not a superset.
        assert!(!matches(
            &h,
            &screen_with(&format!("{}, server:extra", e.channels))
        ));
        assert!(!matches(
            &h,
            &screen_with(&format!("{},server:extra", e.channels))
        ));
    }
}

#[test]
fn the_real_captures_are_answered_by_exactly_their_records() {
    for (screen, channels) in [(REAL_DIALOG_SCREEN, PEERS_ONLY), (TWO_SERVER_SCREEN, BOTH)] {
        assert!(
            screen.contains(&format!("Channels: {channels}\n")),
            "control: capture"
        );
        let answering: Vec<&str> = RECORDS
            .iter()
            .filter(|e| matches(&handler(e), screen))
            .map(|e| e.channels)
            .collect();
        assert!(!answering.is_empty(), "someone answers {channels:?}");
        assert!(
            answering.iter().all(|c| *c == channels),
            "{answering:?} vs {channels:?}"
        );
    }
}

#[test]
fn no_real_record_answers_a_dialog_that_asks_for_a_secret_or_lost_an_anchor() {
    for e in &RECORDS {
        let h = handler(e);
        let screen = screen_with(e.channels);
        assert!(
            matches(&h, &screen),
            "control: {:?} answers its own screen",
            e.channels
        );
        let secret = screen.replace("Esc to cancel", "Enter your password");
        assert_ne!(secret, screen);
        assert!(!matches(&h, &secret));
        for anchor in [
            "WARNING: Loading development channels",
            "I am using this for local development",
        ] {
            assert!(!matches(&h, &screen.replace(anchor, "changed")), "{anchor}");
        }
    }
}
