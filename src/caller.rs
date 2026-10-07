// SPDX-License-Identifier: MIT OR Apache-2.0
//! Who is running this command: a person, or an agent?
//!
//! The portfolio rule is that a start or stop *requested by an agent* is untrusted input and needs a
//! person's consent (DESIGN.md §4.4). This is the detector. **It is a convenience, not a boundary**
//! (DESIGN.md §4.4): an agent can reach this command without a `claude` ancestor (a scheduled task,
//! a detached `wt`), and a lane's role is self-claimed. What protects anything is that the actions
//! an agent may take are narrow (itself, or an idle lane, for the PM) and that everything wider
//! waits for consent (M4). The honest claim is "a casually misbehaving lane cannot get what it
//! should not", the same limit `lane-restart` states of itself.

use crate::hook::HookEnv;
use crate::procindex::ProcIndex;
use crate::registry::AgentId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// No `claude` anywhere above this process: a person's terminal, a scheduled task.
    Person,
    /// Running under a `claude`. `id` is its agent when that process is registered.
    Agent { id: Option<AgentId> },
    /// A `claude` is above us but the walk to it was refused: treated as untrusted, never as a person.
    Unclear(String),
}

pub fn detect(env: &dyn HookEnv, procs: &ProcIndex) -> Caller {
    match env.claude_pid() {
        Ok(pid) => {
            let id = env.start_time(pid).and_then(|s| procs.get(pid, s));
            Caller::Agent { id }
        }
        Err(why) => {
            if env
                .ancestor_images(std::process::id())
                .iter()
                .any(|a| a == "claude")
            {
                Caller::Unclear(why)
            } else {
                Caller::Person
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    struct Env {
        claude: Result<u32, String>,
        start: Option<u64>,
        ancestors: Vec<String>,
    }

    impl HookEnv for Env {
        fn claude_pid(&self) -> Result<u32, String> {
            self.claude.clone()
        }
        fn cmdline(&self, _: u32) -> Option<Vec<String>> {
            None
        }
        fn start_time(&self, _: u32) -> Option<u64> {
            self.start
        }
        fn ancestor_images(&self, _: u32) -> Vec<String> {
            self.ancestors.clone()
        }
        fn var(&self, _: &str) -> Option<String> {
            None
        }
        fn now(&self) -> DateTime<Utc> {
            Utc::now()
        }
    }

    fn images(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_claude_above_is_a_person() {
        let d = tempfile::tempdir().unwrap();
        let procs = ProcIndex::new(d.path());
        let env = Env {
            claude: Err("ancestor process is \"WindowsTerminal.exe\"".into()),
            start: None,
            ancestors: images(&["pwsh", "windowsterminal", "svchost"]),
        };
        assert_eq!(detect(&env, &procs), Caller::Person);
    }

    #[test]
    fn a_registered_claude_above_is_that_agent() {
        let d = tempfile::tempdir().unwrap();
        let procs = ProcIndex::new(d.path());
        procs.put(10, 500, &AgentId::new("a1").unwrap()).unwrap();
        let env = Env {
            claude: Ok(10),
            start: Some(500),
            ancestors: images(&["bash", "claude"]),
        };
        assert_eq!(
            detect(&env, &procs),
            Caller::Agent {
                id: Some(AgentId::new("a1").unwrap())
            }
        );
    }

    #[test]
    fn an_unregistered_claude_above_is_an_agent_without_an_id() {
        let d = tempfile::tempdir().unwrap();
        let procs = ProcIndex::new(d.path());
        let env = Env {
            claude: Ok(10),
            start: Some(500),
            ancestors: images(&["claude"]),
        };
        assert_eq!(detect(&env, &procs), Caller::Agent { id: None });
        // A recycled pid (same pid, different start time) does not resolve to the old agent.
        procs.put(10, 400, &AgentId::new("old").unwrap()).unwrap();
        assert_eq!(detect(&env, &procs), Caller::Agent { id: None });
    }

    #[test]
    fn a_claude_above_with_a_refused_walk_is_never_a_person() {
        let d = tempfile::tempdir().unwrap();
        let procs = ProcIndex::new(d.path());
        let env = Env {
            claude: Err("ancestor process is \"node\"".into()),
            start: None,
            ancestors: images(&["node", "claude", "windowsterminal"]),
        };
        assert!(matches!(detect(&env, &procs), Caller::Unclear(_)));
    }
}
