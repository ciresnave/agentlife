// SPDX-License-Identifier: MIT OR Apache-2.0

//! `status --peers`: which claude-peers broker sessions belong to which role.
//!
//! **The join key is the parent process, never the cwd and never the peer id.** A peer's `pid` is
//! its MCP helper; the helper's parent is the lane's `claude`, and that pid is what the role's
//! lane state records. Two live lanes can share one cwd, and peer ids rotate on every restart.
//!
//! Read-only: it asks the broker for its list and sends nothing. A broker that cannot be read is
//! `"unknown"` for every role, never "no peer".

use crate::notify::Peer;
use serde::Serialize;
use std::collections::BTreeMap;

/// A summary is free text a lane wrote: cut to this many characters before it is printed.
pub const SUMMARY_MAX: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

/// A role's peers: the list, or the string `"unknown"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RolePeers {
    Peers(Vec<PeerRef>),
    Unknown(String),
}

impl RolePeers {
    pub fn unknown() -> Self {
        RolePeers::Unknown("unknown".to_string())
    }
}

/// A peer whose helper's parent is no role's live `claude`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnmatchedPeer {
    pub id: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Joined {
    pub by_role: BTreeMap<String, Vec<PeerRef>>,
    pub unmatched: Vec<UnmatchedPeer>,
}

/// One line, control characters dropped, at most `SUMMARY_MAX` characters (`…` marks a cut).
pub fn clean_summary(s: &str) -> Option<String> {
    let flat: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() <= SUMMARY_MAX {
        return Some(flat);
    }
    let mut cut: String = flat.chars().take(SUMMARY_MAX).collect();
    cut.push('…');
    Some(cut)
}

/// Join `peers` to `live` (role -> the pid of its live `claude`) through `parent_of`.
pub fn join(
    live: &BTreeMap<String, u32>,
    peers: &[Peer],
    parent_of: &dyn Fn(u32) -> Option<u32>,
) -> Joined {
    let mut out = Joined::default();
    for role in live.keys() {
        out.by_role.insert(role.clone(), Vec::new());
    }
    for p in peers {
        let summary = p.summary.as_deref().and_then(clean_summary);
        let parent = if p.pid == 0 { None } else { parent_of(p.pid) };
        let mut matched = false;
        if let Some(parent) = parent {
            for (role, claude) in live {
                if *claude == parent {
                    matched = true;
                    out.by_role
                        .get_mut(role)
                        .expect("every live role was inserted")
                        .push(PeerRef {
                            id: p.id.clone(),
                            summary: summary.clone(),
                            last_seen: p.last_seen.clone(),
                        });
                }
            }
        }
        if !matched {
            out.unmatched.push(UnmatchedPeer {
                id: p.id.clone(),
                cwd: p.cwd.clone(),
                summary,
                last_seen: p.last_seen.clone(),
            });
        }
    }
    out
}

/// Add the peers columns to `report`: each role's list, the unmatched peers, or `"unknown"`.
///
/// Only a role whose recorded pid is still the same live `claude` can own a peer.
pub fn annotate(
    report: &mut crate::spec::StatusReport,
    dir: &std::path::Path,
    facts: &dyn crate::facts::SystemFacts,
    parent_of: &dyn Fn(u32) -> Option<u32>,
    peers: Result<Vec<Peer>, String>,
) {
    let peers = match peers {
        Ok(p) => p,
        Err(e) => {
            for r in &mut report.roles {
                r.peers = Some(RolePeers::unknown());
            }
            report.peers_error = Some(e);
            return;
        }
    };
    let mut live = BTreeMap::new();
    for r in &report.roles {
        if let Ok(st) = crate::state::load(dir, &r.role) {
            if crate::spec::state_process_is_live(&st, facts) {
                live.insert(r.role.clone(), st.pid);
            }
        }
    }
    let joined = join(&live, &peers, parent_of);
    for r in &mut report.roles {
        r.peers = Some(RolePeers::Peers(
            joined.by_role.get(&r.role).cloned().unwrap_or_default(),
        ));
    }
    report.peers_unmatched = Some(joined.unmatched);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, pid: u32, cwd: &str) -> Peer {
        Peer {
            id: id.into(),
            cwd: cwd.into(),
            pid,
            summary: Some("doing x".into()),
            last_seen: Some("t".into()),
        }
    }

    fn live(rows: &[(&str, u32)]) -> BTreeMap<String, u32> {
        rows.iter().map(|(r, p)| (r.to_string(), *p)).collect()
    }

    /// helper pid -> its parent.
    fn parents(rows: &'static [(u32, u32)]) -> impl Fn(u32) -> Option<u32> {
        move |pid| rows.iter().find(|(h, _)| *h == pid).map(|(_, p)| *p)
    }

    #[test]
    fn a_peer_joins_the_role_whose_claude_is_its_helpers_parent() {
        let j = join(
            &live(&[("a", 100), ("b", 200)]),
            &[peer("p1", 11, "C:/x"), peer("p2", 22, "C:/y")],
            &parents(&[(11, 100), (22, 200)]),
        );
        assert_eq!(
            j.by_role["a"].iter().map(|p| &p.id[..]).collect::<Vec<_>>(),
            ["p1"]
        );
        assert_eq!(
            j.by_role["b"].iter().map(|p| &p.id[..]).collect::<Vec<_>>(),
            ["p2"]
        );
        assert!(j.unmatched.is_empty());
    }

    #[test]
    fn two_roles_in_one_cwd_are_joined_by_pid_not_merged() {
        let j = join(
            &live(&[("a", 100), ("b", 200)]),
            &[peer("p1", 11, "C:/same"), peer("p2", 22, "C:/same")],
            &parents(&[(11, 200), (22, 100)]),
        );
        assert_eq!(j.by_role["a"][0].id, "p2");
        assert_eq!(j.by_role["b"][0].id, "p1");
    }

    #[test]
    fn a_role_with_no_peer_is_an_empty_list_not_missing() {
        let j = join(&live(&[("a", 100)]), &[], &parents(&[]));
        assert_eq!(j.by_role["a"], Vec::<PeerRef>::new());
    }

    #[test]
    fn a_live_session_no_state_file_claims_is_unmatched_with_its_cwd() {
        // The `synapse-restarttest-lane` shape: state pid dead, so the role is not in `live`.
        let j = join(
            &live(&[("other", 100)]),
            &[peer("p9", 99, "C:/restarttest")],
            &parents(&[(99, 777)]),
        );
        assert_eq!(j.unmatched.len(), 1);
        assert_eq!(j.unmatched[0].cwd, "C:/restarttest");
        assert!(j.by_role["other"].is_empty());
    }

    #[test]
    fn a_peer_without_a_pid_or_a_parent_is_unmatched_never_guessed_by_cwd() {
        let j = join(
            &live(&[("a", 100)]),
            &[peer("nopid", 0, "C:/a"), peer("orphan", 5, "C:/a")],
            // pid 0 means the broker omitted it: it must not be looked up (here 0 -> 100 would match).
            &parents(&[(0, 100)]),
        );
        assert!(j.by_role["a"].is_empty());
        assert_eq!(j.unmatched.len(), 2);
    }

    #[test]
    fn two_peers_under_one_claude_are_both_listed() {
        let j = join(
            &live(&[("a", 100)]),
            &[peer("p1", 11, "C:/a"), peer("p2", 12, "C:/a")],
            &parents(&[(11, 100), (12, 100)]),
        );
        assert_eq!(j.by_role["a"].len(), 2);
    }

    #[test]
    fn a_summary_is_one_line_without_control_characters_and_cut() {
        assert_eq!(
            clean_summary("a\n\tb\x1b[31m  c").as_deref(),
            Some("a b[31m c")
        );
        assert_eq!(clean_summary("  \n "), None);
        let long = "x".repeat(SUMMARY_MAX + 40);
        let cut = clean_summary(&long).unwrap();
        assert_eq!(cut.chars().count(), SUMMARY_MAX + 1);
        assert!(cut.ends_with('…'));
        let exact = "y".repeat(SUMMARY_MAX);
        assert_eq!(clean_summary(&exact).unwrap(), exact);
    }

    #[test]
    fn unknown_serialises_as_the_bare_string_and_a_list_as_an_array() {
        assert_eq!(
            serde_json::to_string(&RolePeers::unknown()).unwrap(),
            "\"unknown\""
        );
        let l = RolePeers::Peers(vec![PeerRef {
            id: "i".into(),
            summary: None,
            last_seen: None,
        }]);
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"[{"id":"i"}]"#);
    }
}
