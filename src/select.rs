// SPDX-License-Identifier: MIT OR Apache-2.0
//! Naming an agent on the command line: by id, or by name.

use crate::registry::AgentRecord;
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    NotFound(String),
    /// Several agents have that name; the ids to choose between.
    Ambiguous {
        selector: String,
        ids: Vec<String>,
    },
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectError::NotFound(s) => write!(f, "no agent with id or name {s:?}"),
            SelectError::Ambiguous { selector, ids } => write!(
                f,
                "{selector:?} names {} agents; use the id: {}",
                ids.len(),
                ids.join(", ")
            ),
        }
    }
}

impl std::error::Error for SelectError {}

/// An exact agent id wins; otherwise a name, ignoring case. A name shared by several agents
/// (including closed ones) is refused rather than guessed: acting on the wrong agent is the worse
/// error.
pub fn resolve<'a>(
    selector: &str,
    records: &'a [AgentRecord],
) -> Result<&'a AgentRecord, SelectError> {
    if let Some(r) = records.iter().find(|r| r.agent_id.as_str() == selector) {
        return Ok(r);
    }
    let named: Vec<&AgentRecord> = records
        .iter()
        .filter(|r| {
            r.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(selector))
        })
        .collect();
    match named.as_slice() {
        [] => Err(SelectError::NotFound(selector.to_string())),
        [one] => Ok(one),
        many => Err(SelectError::Ambiguous {
            selector: selector.to_string(),
            ids: many.iter().map(|r| r.agent_id.to_string()).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AgentId;
    use chrono::{TimeZone, Utc};

    fn rec(id: &str, name: Option<&str>) -> AgentRecord {
        let mut r = AgentRecord::new(
            AgentId::new(id).unwrap(),
            "C:/x",
            Utc.with_ymd_and_hms(2026, 10, 7, 1, 0, 0).unwrap(),
        );
        r.name = name.map(String::from);
        r
    }

    #[test]
    fn an_id_wins_and_a_name_matches_ignoring_case() {
        let rs = vec![
            rec("a1", Some("synapse")),
            rec("a2", Some("PM")),
            rec("a3", None),
        ];
        assert_eq!(resolve("a2", &rs).unwrap().agent_id.as_str(), "a2");
        assert_eq!(resolve("pm", &rs).unwrap().agent_id.as_str(), "a2");
        assert_eq!(resolve("SYNAPSE", &rs).unwrap().agent_id.as_str(), "a1");
        assert_eq!(resolve("a3", &rs).unwrap().agent_id.as_str(), "a3");
    }

    #[test]
    fn an_id_that_is_also_someones_name_resolves_to_the_id() {
        let rs = vec![rec("a1", Some("a2")), rec("a2", Some("other"))];
        assert_eq!(resolve("a2", &rs).unwrap().agent_id.as_str(), "a2");
    }

    #[test]
    fn a_shared_name_is_refused_with_the_ids_to_choose_from() {
        let rs = vec![rec("a1", Some("lane")), rec("a2", Some("Lane"))];
        match resolve("lane", &rs) {
            Err(SelectError::Ambiguous { ids, .. }) => assert_eq!(ids, ["a1", "a2"]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unknown_selector_is_not_found() {
        let rs = vec![rec("a1", Some("x"))];
        assert_eq!(
            resolve("nope", &rs),
            Err(SelectError::NotFound("nope".into()))
        );
        assert!(resolve("x", &[]).is_err());
    }
}
