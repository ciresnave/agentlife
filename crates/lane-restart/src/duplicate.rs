// SPDX-License-Identifier: MIT OR Apache-2.0

//! The double-launch guard (PM task 2026-10-14): a role must not get a second session while a
//! first one is still in the process table, whatever its state file says.
//!
//! The 2026-10-14 case: a lane did `--self` at 09:03, and the PM ran `start --role unisql` at
//! 09:11 while the first host and claude were alive but stuck at a startup dialog. The state file
//! names a session only once its `SessionStart` hook has fired, so it named neither. The process
//! table is the only record that sees such a session, so this module reads that and nothing else:
//!
//! * a `lane-restart host --role <r>` process, and
//! * a `claude` whose launch arguments carry `--name <r>` (or the spec's own name for the role).
//!
//! Both are matched by their command line, never by cwd (a role's directory is shared with
//! sub-directory roles). The decision is pure over a process table so it is testable without a
//! window; `check` is the one place that asks the facts for one.

use crate::facts::{ProcEntry, SystemFacts};
use serde::Serialize;
use std::collections::HashSet;

/// A process that already is, or hosts, a session of the role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Duplicate {
    pub pid: u32,
    /// `host` (a `lane-restart host --role R`) or `claude` (a claude named for R).
    pub kind: &'static str,
    pub start_time_secs: u64,
}

impl std::fmt::Display for Duplicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} pid {} (started {})",
            self.kind, self.pid, self.start_time_secs
        )
    }
}

/// Why a launch of the role was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateRefusal {
    /// Live processes already belong to the role.
    Live(Vec<Duplicate>),
    /// The process table could not be read, so "no duplicate" cannot be said.
    Unreadable(String),
}

impl std::fmt::Display for DuplicateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DuplicateRefusal::Live(v) => {
                let list: Vec<String> = v.iter().map(ToString::to_string).collect();
                write!(
                    f,
                    "duplicate-risk: the process table already holds a session of this role \
                     ({}); a second one would run beside it. Close that session (or its \
                     startup dialog) first, then retry",
                    list.join(", ")
                )
            }
            DuplicateRefusal::Unreadable(why) => write!(
                f,
                "duplicate-risk: the process table could not be read ({why}), so a live session \
                 of this role cannot be ruled out"
            ),
        }
    }
}

use lane_state::platform::names::image_base as base_name;

fn is_host_of(p: &ProcEntry, role: &str) -> bool {
    base_name(&p.name) == "lane-restart"
        && p.cmd
            .windows(3)
            .any(|w| w[0] == "host" && w[1] == "--role" && w[2] == role)
}

fn is_claude_named(p: &ProcEntry, names: &[&str]) -> bool {
    if base_name(&p.name) != "claude" {
        return false;
    }
    p.cmd.iter().enumerate().any(|(i, a)| {
        let value = match a.strip_prefix("--name=") {
            Some(v) => Some(v),
            None if a == "--name" => p.cmd.get(i + 1).map(String::as_str),
            None => None,
        };
        value.is_some_and(|v| names.contains(&v))
    })
}

/// Every process of `role` in `table`, except `expected` and what `expected` runs under.
///
/// `expected` is the session a restart is about to replace: its own pid and its ancestors (its
/// host) are what the restart legitimately kills, so they are not duplicates. Everything else
/// that matches is.
pub fn find(
    table: &[ProcEntry],
    role: &str,
    names: &[&str],
    expected: Option<u32>,
) -> Vec<Duplicate> {
    let mut skip: HashSet<u32> = HashSet::new();
    let mut cur = expected;
    while let Some(pid) = cur {
        // a cycle in a recycled-pid table must not hang the walk
        if !skip.insert(pid) {
            break;
        }
        cur = table.iter().find(|p| p.pid == pid).and_then(|p| p.parent);
    }
    let mut out: Vec<Duplicate> = table
        .iter()
        .filter(|p| !skip.contains(&p.pid))
        .filter_map(|p| {
            let kind = if is_host_of(p, role) {
                "host"
            } else if is_claude_named(p, names) {
                "claude"
            } else {
                return None;
            };
            Some(Duplicate {
                pid: p.pid,
                kind,
                start_time_secs: p.start_time_secs,
            })
        })
        .collect();
    out.sort_by_key(|d| d.pid);
    out
}

/// The names a claude of `role` may carry in `--name`: the role itself and, when the role has a
/// readable launch spec, the name that spec launches under.
pub fn role_names(dir: &std::path::Path, role: &str) -> Vec<String> {
    let mut names = vec![role.to_string()];
    if let Ok(spec) = crate::spec::read(dir, role) {
        let n = spec.launch_spec().name.to_string();
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

/// One process-table snapshot, then `find`. A table that cannot be read refuses: a guard that
/// says "none" when it could not look would be worse than none.
pub fn check(
    facts: &dyn SystemFacts,
    role: &str,
    names: &[&str],
    expected: Option<u32>,
) -> Result<(), DuplicateRefusal> {
    let table = facts
        .process_table()
        .map_err(|e| DuplicateRefusal::Unreadable(format!("{e:?}")))?;
    let found = find(&table, role, names, expected);
    if found.is_empty() {
        Ok(())
    } else {
        Err(DuplicateRefusal::Live(found))
    }
}

/// `status`: marks each role whose process table holds a session the role's own live state file
/// does not account for, and clears its `startable`. A running lane's host and claude are
/// accounted for (the state's pid and its ancestors), so a normal row is never marked.
pub fn annotate(
    report: &mut crate::spec::StatusReport,
    dir: &std::path::Path,
    facts: &dyn SystemFacts,
) {
    let table = match facts.process_table() {
        Ok(t) => t,
        Err(e) => {
            report.duplicate_error = Some(format!("{e:?}"));
            return;
        }
    };
    for r in &mut report.roles {
        let live_pid = crate::state::load(dir, &r.role)
            .ok()
            .filter(|st| crate::spec::state_process_is_live(st, facts))
            .map(|st| st.pid);
        let names = role_names(dir, &r.role);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let found = find(&table, &r.role, &names, live_pid);
        if !found.is_empty() {
            r.startable = false;
            r.duplicate_risk = found;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{log, RecordingFacts};

    fn p(pid: u32, parent: Option<u32>, name: &str, cmd: &[&str]) -> ProcEntry {
        ProcEntry {
            pid,
            parent,
            name: name.to_string(),
            start_time_secs: 1000 + u64::from(pid),
            exe: None,
            cmd: cmd.iter().map(ToString::to_string).collect(),
        }
    }

    fn host(pid: u32, role: &str) -> ProcEntry {
        p(
            pid,
            Some(1),
            "lane-restart.exe",
            &[
                "C:/bin/lane-restart.exe",
                "host",
                "--role",
                role,
                "--",
                "claude.exe",
            ],
        )
    }

    fn claude(pid: u32, parent: u32, name: &str) -> ProcEntry {
        p(
            pid,
            Some(parent),
            "claude.exe",
            &[
                "claude.exe",
                "--model",
                "sonnet",
                "--name",
                name,
                "read HANDOFF",
            ],
        )
    }

    /// 09:11 case: host and claude alive, stuck at the startup dialog; no state file at all.
    #[test]
    fn a_stuck_at_dialog_host_and_claude_are_both_found_with_no_state_file() {
        let table = vec![host(100, "unisql"), claude(101, 100, "unisql")];
        let d = find(&table, "unisql", &["unisql"], None);
        assert_eq!(
            d.iter().map(|x| (x.pid, x.kind)).collect::<Vec<_>>(),
            vec![(100, "host"), (101, "claude")]
        );
    }

    /// Control: a table with other roles, a plain shell and a differently named claude is clean.
    #[test]
    fn other_roles_and_unrelated_processes_are_not_duplicates() {
        let table = vec![
            host(100, "baracuda"),
            claude(101, 100, "baracuda"),
            claude(102, 1, "unisql-docs"),
            p(103, Some(1), "pwsh.exe", &["pwsh.exe", "--name", "unisql"]),
        ];
        assert_eq!(find(&table, "unisql", &["unisql"], None), vec![]);
        // the same table does find the role that IS there
        assert_eq!(find(&table, "baracuda", &["baracuda"], None).len(), 2);
    }

    #[test]
    fn the_equals_form_of_name_and_the_specs_own_name_both_match() {
        let table = vec![p(7, Some(1), "claude", &["claude", "--name=Unisql Lane"])];
        assert_eq!(find(&table, "unisql", &["unisql"], None), vec![]);
        assert_eq!(
            find(&table, "unisql", &["unisql", "Unisql Lane"], None).len(),
            1
        );
    }

    /// A restart replaces its own session: that claude and its host are expected, a second host
    /// is not.
    #[test]
    fn a_restart_ignores_its_own_session_and_host_but_not_another_one() {
        let own = vec![host(100, "unisql"), claude(101, 100, "unisql")];
        assert_eq!(find(&own, "unisql", &["unisql"], Some(101)), vec![]);
        let mut two = own.clone();
        two.push(host(200, "unisql"));
        two.push(claude(201, 200, "unisql"));
        let d = find(&two, "unisql", &["unisql"], Some(101));
        assert_eq!(d.iter().map(|x| x.pid).collect::<Vec<_>>(), vec![200, 201]);
    }

    #[test]
    fn a_parent_cycle_does_not_hang_the_walk() {
        let table = vec![
            p(1, Some(2), "a", &[]),
            p(2, Some(1), "b", &[]),
            claude(3, 1, "x"),
        ];
        assert_eq!(find(&table, "r", &["r"], Some(1)), vec![]);
    }

    #[test]
    fn check_refuses_on_a_live_host_and_names_the_pids() {
        let l = log();
        let mut f = RecordingFacts::new(&l);
        f.table = Ok(vec![host(100, "unisql")]);
        let r = check(&f, "unisql", &["unisql"], None).unwrap_err();
        assert!(matches!(r, DuplicateRefusal::Live(_)));
        assert!(r.to_string().contains("host pid 100"), "{r}");
        // control: the same call on an empty table passes
        f.table = Ok(vec![]);
        assert_eq!(check(&f, "unisql", &["unisql"], None), Ok(()));
    }

    fn write_state(dir: &std::path::Path, role: &str, pid: u32, start: u64) {
        let mut st = crate::testing::state_at(role, "s", "PreToolUse");
        st.pid = pid;
        st.pid_start_secs = Some(start);
        std::fs::write(
            dir.join(format!("{role}.json")),
            serde_json::to_string(&st).unwrap(),
        )
        .unwrap();
    }

    /// `status`: a role running normally (its host and claude are what its state records) is
    /// not marked; a role whose state is stale beside a live host is, and is not startable.
    #[test]
    fn status_marks_a_hidden_session_and_leaves_a_normal_row_alone() {
        let dir = tempfile::tempdir().unwrap();
        write_state(dir.path(), "baracuda", 101, 1101);
        write_state(dir.path(), "unisql", 999, 1);
        let l = log();
        let mut f = RecordingFacts::new(&l);
        f.live_pid = Some((101, 1101));
        f.table = Ok(vec![
            host(100, "baracuda"),
            claude(101, 100, "baracuda"),
            host(200, "unisql"),
            claude(201, 200, "unisql"),
        ]);
        let mut report = crate::spec::status_all(dir.path(), &f);
        annotate(&mut report, dir.path(), &f);
        let by = |r: &str| report.roles.iter().find(|x| x.role == r).unwrap().clone();
        assert!(
            by("baracuda").duplicate_risk.is_empty(),
            "{:?}",
            by("baracuda")
        );
        let u = by("unisql");
        assert_eq!(
            u.duplicate_risk.iter().map(|x| x.pid).collect::<Vec<_>>(),
            vec![200, 201]
        );
        assert!(!u.startable);
    }

    #[test]
    fn an_unreadable_table_refuses_rather_than_passing() {
        let l = log();
        let mut f = RecordingFacts::new(&l);
        f.table = Err("denied".into());
        let r = check(&f, "unisql", &["unisql"], None);
        assert!(matches!(r, Err(DuplicateRefusal::Unreadable(_))), "{r:?}");
    }
}
