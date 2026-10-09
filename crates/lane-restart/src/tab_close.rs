// SPDX-License-Identifier: MIT OR Apache-2.0

//! Closing the terminal tab a killed lane leaves behind - RESTART-TOOL-DESIGN.md §5.
//!
//! CireSnave, 2026-09-27 (via the PM): "Our restart tool isn't closing the leftover terminal
//! window from the old agents." A hand-started lane runs `WindowsTerminal -> pwsh -> claude`.
//! Killing `claude` leaves `pwsh` alive at a prompt, so the tab stays open. (A tool-launched lane,
//! `wt -> lane-restart host -> claude`, has no shell underneath and already closes on its own.)
//!
//! The rule: after the kill, terminate claude's DIRECT parent only if it is an interactive shell
//! (`pwsh`/`powershell`/`cmd`) whose own parent is a terminal (`WindowsTerminal`/`OpenConsole`),
//! and only if that shell has no other children once claude is gone. Anything else is left alone,
//! with the reason logged.
//!
//! ⚠️ NEVER BY NAME ALONE (§2). Windows never rewrites a process's recorded parent pid, so after a
//! parent exits its pid can be reused by something unrelated that still looks like the "parent".
//! A recorded parent only counts if it started no later than its child, and the shell is captured
//! with its full `ProcessIdentity` while claude is still alive, then re-verified before the kill.
//!
//! The decisions are pure functions over a `ProcEntry` table, so every case is tested against a
//! fake table; `close` performs the one real kill through `SystemFacts::kill_verified`.

use crate::facts::{ProcEntry, ProcessIdentity, SystemFacts};

/// Interactive shells whose tab closes when they exit (lowercased, no `.exe`).
const TERMINAL_SHELLS: &[&str] = &["pwsh", "powershell", "cmd"];
/// The terminal processes that host such a shell's tab.
const TERMINAL_HOSTS: &[&str] = &["windowsterminal", "openconsole"];

/// A shell that looked closable while claude was still alive - captured then, re-verified later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCandidate {
    pub pid: u32,
    pub name: String,
    pub identity: ProcessIdentity,
    pub terminal_pid: u32,
    pub terminal_name: String,
}

impl std::fmt::Display for ShellCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} pid {} (under {} pid {})",
            self.name, self.pid, self.terminal_name, self.terminal_pid
        )
    }
}

/// Why the leftover shell was left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeftAlone {
    EnumerationFailed(String),
    ClaudeNotFound(u32),
    ClaudeIdentityChanged(u32),
    NoParent,
    ParentGone(u32),
    /// The recorded parent pid belongs to a process that started after claude did - a reused
    /// pid, not claude's real parent.
    ParentPidReused(u32),
    ParentNotAShell {
        pid: u32,
        name: String,
    },
    TerminalGone(u32),
    TerminalPidReused(u32),
    ShellParentNotATerminal {
        pid: u32,
        name: String,
    },
    ShellGone(u32),
    ShellIdentityChanged(u32),
    ShellHasOtherChildren(Vec<(u32, String)>),
    RelaunchFailed,
    KillRefused(String),
}

impl std::fmt::Display for LeftAlone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeftAlone::EnumerationFailed(e) => {
                write!(f, "the process table could not be read: {e}")
            }
            LeftAlone::ClaudeNotFound(p) => {
                write!(f, "claude pid {p} was not in the process table")
            }
            LeftAlone::ClaudeIdentityChanged(p) => {
                write!(
                    f,
                    "claude pid {p}'s identity no longer matched the recorded one"
                )
            }
            LeftAlone::NoParent => write!(f, "claude has no recorded parent"),
            LeftAlone::ParentGone(p) => write!(f, "claude's parent pid {p} had already exited"),
            LeftAlone::ParentPidReused(p) => {
                write!(
                    f,
                    "parent pid {p} started after claude - a reused pid, not its parent"
                )
            }
            LeftAlone::ParentNotAShell { pid, name } => {
                write!(
                    f,
                    "claude's parent {name} pid {pid} is not an interactive shell"
                )
            }
            LeftAlone::TerminalGone(p) => {
                write!(f, "the shell's parent pid {p} had already exited")
            }
            LeftAlone::TerminalPidReused(p) => {
                write!(
                    f,
                    "the shell's parent pid {p} started after the shell - a reused pid"
                )
            }
            LeftAlone::ShellParentNotATerminal { pid, name } => {
                write!(f, "the shell's parent {name} pid {pid} is not a terminal")
            }
            LeftAlone::ShellGone(p) => write!(f, "shell pid {p} had already exited"),
            LeftAlone::ShellIdentityChanged(p) => {
                write!(f, "shell pid {p}'s identity changed since it was captured")
            }
            LeftAlone::ShellHasOtherChildren(children) => {
                let list: Vec<String> = children
                    .iter()
                    .map(|(pid, name)| format!("{name} pid {pid}"))
                    .collect();
                write!(f, "the shell still has other children: {}", list.join(", "))
            }
            LeftAlone::RelaunchFailed => {
                write!(
                    f,
                    "the relaunch failed, so the tab stays for a human to relaunch from"
                )
            }
            LeftAlone::KillRefused(e) => write!(f, "terminating the shell was refused: {e}"),
        }
    }
}

fn base_name(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn entry(table: &[ProcEntry], pid: u32) -> Option<&ProcEntry> {
    table.iter().find(|p| p.pid == pid)
}

/// Captured while claude is still alive, right before the kill.
pub fn find_candidate(
    table: &[ProcEntry],
    claude_pid: u32,
    claude_identity: &ProcessIdentity,
) -> Result<ShellCandidate, LeftAlone> {
    let claude = entry(table, claude_pid).ok_or(LeftAlone::ClaudeNotFound(claude_pid))?;
    if claude.start_time_secs != claude_identity.start_time_secs {
        return Err(LeftAlone::ClaudeIdentityChanged(claude_pid));
    }
    let shell_pid = claude.parent.ok_or(LeftAlone::NoParent)?;
    let shell = entry(table, shell_pid).ok_or(LeftAlone::ParentGone(shell_pid))?;
    if shell.start_time_secs > claude.start_time_secs {
        return Err(LeftAlone::ParentPidReused(shell_pid));
    }
    let shell_name = base_name(&shell.name);
    if !TERMINAL_SHELLS.contains(&shell_name.as_str()) {
        return Err(LeftAlone::ParentNotAShell {
            pid: shell_pid,
            name: shell_name,
        });
    }
    let terminal_pid = shell.parent.ok_or(LeftAlone::ShellParentNotATerminal {
        pid: 0,
        name: "(none recorded)".into(),
    })?;
    let terminal = entry(table, terminal_pid).ok_or(LeftAlone::TerminalGone(terminal_pid))?;
    if terminal.start_time_secs > shell.start_time_secs {
        return Err(LeftAlone::TerminalPidReused(terminal_pid));
    }
    let terminal_name = base_name(&terminal.name);
    if !TERMINAL_HOSTS.contains(&terminal_name.as_str()) {
        return Err(LeftAlone::ShellParentNotATerminal {
            pid: terminal_pid,
            name: terminal_name,
        });
    }
    Ok(ShellCandidate {
        pid: shell_pid,
        name: shell_name,
        identity: ProcessIdentity {
            start_time_secs: shell.start_time_secs,
            exe: shell.exe.clone(),
        },
        terminal_pid,
        terminal_name,
    })
}

/// Re-checked after the kill: the shell must be the same process, and have no children left
/// other than the claude that was just killed (which may still be listed while it tears down).
pub fn still_closable(
    table: &[ProcEntry],
    candidate: &ShellCandidate,
    killed_claude_pid: u32,
    killed_claude_identity: &ProcessIdentity,
) -> Result<(), LeftAlone> {
    let shell = entry(table, candidate.pid).ok_or(LeftAlone::ShellGone(candidate.pid))?;
    let now = ProcessIdentity {
        start_time_secs: shell.start_time_secs,
        exe: shell.exe.clone(),
    };
    if now.start_time_secs != candidate.identity.start_time_secs
        || !crate::facts::exe_matches(&now.exe, &candidate.identity.exe)
    {
        return Err(LeftAlone::ShellIdentityChanged(candidate.pid));
    }
    let others: Vec<(u32, String)> = table
        .iter()
        .filter(|p| p.parent == Some(candidate.pid))
        // A child can't predate its parent: an earlier start means a stale parent pid left over
        // from a previous owner of this pid.
        .filter(|p| p.start_time_secs >= shell.start_time_secs)
        .filter(|p| {
            !(p.pid == killed_claude_pid
                && p.start_time_secs == killed_claude_identity.start_time_secs)
        })
        .map(|p| (p.pid, base_name(&p.name)))
        .collect();
    if !others.is_empty() {
        return Err(LeftAlone::ShellHasOtherChildren(others));
    }
    Ok(())
}

/// Captures the candidate from the live process table.
pub fn capture(
    facts: &dyn SystemFacts,
    claude_pid: u32,
    claude_identity: &ProcessIdentity,
) -> Result<ShellCandidate, LeftAlone> {
    let table = facts
        .process_table()
        .map_err(|e| LeftAlone::EnumerationFailed(format!("{e:?}")))?;
    find_candidate(&table, claude_pid, claude_identity)
}

/// Re-verifies `candidate` against a fresh table and, if it is still closable, calls
/// `before_kill` (so the log line is written even if closing the tab takes this process with
/// it) and then terminates the shell through `kill_verified`.
pub fn close(
    facts: &dyn SystemFacts,
    candidate: &ShellCandidate,
    killed_claude_pid: u32,
    killed_claude_identity: &ProcessIdentity,
    before_kill: &mut dyn FnMut(&ShellCandidate),
) -> Result<(), LeftAlone> {
    let table = facts
        .process_table()
        .map_err(|e| LeftAlone::EnumerationFailed(format!("{e:?}")))?;
    still_closable(&table, candidate, killed_claude_pid, killed_claude_identity)?;
    before_kill(candidate);
    facts
        .kill_verified(candidate.pid, &candidate.identity)
        .map_err(|e| LeftAlone::KillRefused(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{KillError, ShellCheckError};
    use std::cell::RefCell;
    use std::path::PathBuf;

    const TERMINAL: u32 = 2812;
    const SHELL: u32 = 16500;
    const CLAUDE: u32 = 4200;

    fn proc(pid: u32, parent: Option<u32>, name: &str, start: u64) -> ProcEntry {
        ProcEntry {
            pid,
            parent,
            name: name.to_string(),
            start_time_secs: start,
            exe: Some(PathBuf::from(format!(r"C:\bin\{name}"))),
        }
    }

    fn identity_of(p: &ProcEntry) -> ProcessIdentity {
        ProcessIdentity {
            start_time_secs: p.start_time_secs,
            exe: p.exe.clone(),
        }
    }

    /// The live shape the PM measured: WindowsTerminal(2812) -> pwsh -> claude.
    fn hand_started() -> Vec<ProcEntry> {
        vec![
            proc(TERMINAL, Some(1), "WindowsTerminal.exe", 100),
            proc(SHELL, Some(TERMINAL), "pwsh.exe", 200),
            proc(CLAUDE, Some(SHELL), "claude.exe", 300),
        ]
    }

    fn claude_identity(table: &[ProcEntry]) -> ProcessIdentity {
        identity_of(entry(table, CLAUDE).unwrap())
    }

    fn without(table: &[ProcEntry], pid: u32) -> Vec<ProcEntry> {
        table.iter().filter(|p| p.pid != pid).cloned().collect()
    }

    // -- find_candidate --------------------------------------------------------------------- //

    #[test]
    fn a_hand_started_lane_yields_its_pwsh_as_the_candidate() {
        let table = hand_started();
        let c = find_candidate(&table, CLAUDE, &claude_identity(&table)).unwrap();
        assert_eq!(c.pid, SHELL);
        assert_eq!(c.name, "pwsh");
        assert_eq!(c.identity, identity_of(entry(&table, SHELL).unwrap()));
        assert_eq!(
            (c.terminal_pid, c.terminal_name.as_str()),
            (TERMINAL, "windowsterminal")
        );
    }

    #[test]
    fn powershell_and_cmd_under_openconsole_are_candidates_too() {
        for shell in ["powershell.exe", "cmd.exe", "PWSH.EXE"] {
            let table = vec![
                proc(TERMINAL, Some(1), "OpenConsole.exe", 100),
                proc(SHELL, Some(TERMINAL), shell, 200),
                proc(CLAUDE, Some(SHELL), "claude.exe", 300),
            ];
            let c = find_candidate(&table, CLAUDE, &claude_identity(&table));
            assert!(
                c.is_ok(),
                "{shell} under OpenConsole must be a candidate, got {c:?}"
            );
        }
    }

    #[test]
    fn a_tool_launched_lane_under_lane_restart_host_is_left_alone() {
        let table = vec![
            proc(TERMINAL, Some(1), "WindowsTerminal.exe", 100),
            proc(SHELL, Some(TERMINAL), "lane-restart.exe", 200),
            proc(CLAUDE, Some(SHELL), "claude.exe", 300),
        ];
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ParentNotAShell {
                pid: SHELL,
                name: "lane-restart".into()
            })
        );
    }

    #[test]
    fn a_non_interactive_shell_like_bash_is_left_alone() {
        let mut table = hand_started();
        table[1].name = "bash.exe".into();
        assert!(matches!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ParentNotAShell { .. })
        ));
    }

    #[test]
    fn a_shell_whose_parent_is_not_a_terminal_is_left_alone() {
        let mut table = hand_started();
        table[0].name = "Code.exe".into();
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ShellParentNotATerminal {
                pid: TERMINAL,
                name: "code".into()
            })
        );
    }

    #[test]
    fn a_parent_that_already_exited_is_left_alone() {
        let table = without(&hand_started(), SHELL);
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ParentGone(SHELL))
        );
    }

    /// ⚠️ The §2 case: claude's real pwsh exited, and a NEW pwsh reused its pid. Same pid, right
    /// name, right grandparent - but it started after claude, so it cannot be claude's parent.
    #[test]
    fn a_reused_parent_pid_that_started_after_claude_is_left_alone() {
        let mut table = hand_started();
        table[1].start_time_secs = 400;
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ParentPidReused(SHELL))
        );
    }

    #[test]
    fn a_reused_terminal_pid_that_started_after_the_shell_is_left_alone() {
        let mut table = hand_started();
        table[0].start_time_secs = 250;
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::TerminalPidReused(TERMINAL))
        );
    }

    #[test]
    fn a_shell_whose_terminal_already_exited_is_left_alone() {
        let table = without(&hand_started(), TERMINAL);
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::TerminalGone(TERMINAL))
        );
    }

    #[test]
    fn claude_without_a_recorded_parent_is_left_alone() {
        let mut table = hand_started();
        table[2].parent = None;
        assert_eq!(
            find_candidate(&table, CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::NoParent)
        );
    }

    #[test]
    fn a_claude_whose_identity_changed_is_left_alone() {
        let table = hand_started();
        let wrong = ProcessIdentity {
            start_time_secs: 999,
            exe: None,
        };
        assert_eq!(
            find_candidate(&table, CLAUDE, &wrong),
            Err(LeftAlone::ClaudeIdentityChanged(CLAUDE))
        );
        assert_eq!(
            find_candidate(&without(&table, CLAUDE), CLAUDE, &claude_identity(&table)),
            Err(LeftAlone::ClaudeNotFound(CLAUDE))
        );
    }

    // -- still_closable --------------------------------------------------------------------- //

    fn captured() -> (Vec<ProcEntry>, ShellCandidate, ProcessIdentity) {
        let table = hand_started();
        let id = claude_identity(&table);
        let c = find_candidate(&table, CLAUDE, &id).unwrap();
        (table, c, id)
    }

    #[test]
    fn a_shell_left_childless_after_the_kill_is_closable() {
        let (table, c, id) = captured();
        assert_eq!(
            still_closable(&without(&table, CLAUDE), &c, CLAUDE, &id),
            Ok(())
        );
    }

    /// The killed claude may still be listed for a moment while it tears down; it is not
    /// "another child".
    #[test]
    fn the_killed_claude_still_being_listed_does_not_count_as_another_child() {
        let (table, c, id) = captured();
        assert_eq!(still_closable(&table, &c, CLAUDE, &id), Ok(()));
    }

    /// ⚠️ THE NEGATIVE THE PM ASKED FOR: a shell with anything else still running under it is
    /// someone's live work, never closed.
    #[test]
    fn a_shell_that_has_another_child_is_left_alone() {
        let (table, c, id) = captured();
        let mut after = without(&table, CLAUDE);
        after.push(proc(7777, Some(SHELL), "cargo.exe", 350));
        assert_eq!(
            still_closable(&after, &c, CLAUDE, &id),
            Err(LeftAlone::ShellHasOtherChildren(vec![(
                7777,
                "cargo".into()
            )]))
        );
    }

    /// A NEW claude that happened to reuse the killed one's pid is a different process, so it
    /// counts as another child.
    #[test]
    fn a_new_process_reusing_the_killed_claudes_pid_counts_as_another_child() {
        let (table, c, id) = captured();
        let mut after = without(&table, CLAUDE);
        after.push(proc(CLAUDE, Some(SHELL), "claude.exe", 500));
        assert!(matches!(
            still_closable(&after, &c, CLAUDE, &id),
            Err(LeftAlone::ShellHasOtherChildren(_))
        ));
    }

    /// A process whose stale parent pid points at the shell's pid but which started BEFORE the
    /// shell is not the shell's child (its real parent was an earlier owner of that pid).
    #[test]
    fn a_stale_parent_pid_from_before_the_shell_started_is_not_a_child() {
        let (table, c, id) = captured();
        let mut after = without(&table, CLAUDE);
        after.push(proc(8888, Some(SHELL), "svchost.exe", 50));
        assert_eq!(still_closable(&after, &c, CLAUDE, &id), Ok(()));
    }

    #[test]
    fn a_shell_that_exited_after_capture_is_left_alone() {
        let (table, c, id) = captured();
        let after = without(&without(&table, CLAUDE), SHELL);
        assert_eq!(
            still_closable(&after, &c, CLAUDE, &id),
            Err(LeftAlone::ShellGone(SHELL))
        );
    }

    #[test]
    fn a_shell_pid_reused_after_capture_is_left_alone() {
        let (table, c, id) = captured();
        let mut after = without(&table, CLAUDE);
        after
            .iter_mut()
            .find(|p| p.pid == SHELL)
            .unwrap()
            .start_time_secs = 600;
        assert_eq!(
            still_closable(&after, &c, CLAUDE, &id),
            Err(LeftAlone::ShellIdentityChanged(SHELL))
        );
    }

    // -- close (the one real kill, through SystemFacts) ------------------------------------- //

    struct FakeFacts {
        table: Result<Vec<ProcEntry>, ShellCheckError>,
        killed: RefCell<Vec<(u32, ProcessIdentity)>>,
    }

    impl SystemFacts for FakeFacts {
        fn is_alive_claude_process(&self, _: u32) -> bool {
            unreachable!()
        }
        fn cwd_of(&self, _: u32) -> Option<PathBuf> {
            unreachable!()
        }
        fn has_live_shell_descendant(&self, _: u32) -> Result<bool, ShellCheckError> {
            unreachable!()
        }
        fn transcript_is_recent(&self, _: &str, _: &str, _: std::time::Duration) -> bool {
            unreachable!()
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            unreachable!()
        }
        fn process_identity(&self, _: u32) -> Option<ProcessIdentity> {
            unreachable!()
        }
        fn kill_verified(&self, pid: u32, expected: &ProcessIdentity) -> Result<(), KillError> {
            self.killed.borrow_mut().push((pid, expected.clone()));
            Ok(())
        }
        fn find_claude_process_in(&self, _: &str, _: u64) -> Option<u32> {
            unreachable!()
        }
        fn process_table(&self) -> Result<Vec<ProcEntry>, ShellCheckError> {
            self.table.clone()
        }
    }

    #[test]
    fn close_kills_exactly_the_captured_shell_and_logs_it_first() {
        let (table, c, id) = captured();
        let facts = FakeFacts {
            table: Ok(without(&table, CLAUDE)),
            killed: RefCell::new(vec![]),
        };
        let mut logged = vec![];
        assert_eq!(
            close(&facts, &c, CLAUDE, &id, &mut |c| logged.push(c.pid)),
            Ok(())
        );
        assert_eq!(logged, vec![SHELL]);
        assert_eq!(*facts.killed.borrow(), vec![(SHELL, c.identity.clone())]);
    }

    #[test]
    fn close_never_kills_a_shell_with_another_child() {
        let (table, c, id) = captured();
        let mut after = without(&table, CLAUDE);
        after.push(proc(7777, Some(SHELL), "cargo.exe", 350));
        let facts = FakeFacts {
            table: Ok(after),
            killed: RefCell::new(vec![]),
        };
        let mut logged = vec![];
        assert!(close(&facts, &c, CLAUDE, &id, &mut |c| logged.push(c.pid)).is_err());
        assert!(logged.is_empty());
        assert!(
            facts.killed.borrow().is_empty(),
            "a refused close must never signal"
        );
    }

    #[test]
    fn close_never_kills_when_the_table_cannot_be_read() {
        let (_, c, id) = captured();
        let facts = FakeFacts {
            table: Err(ShellCheckError::EnumerationFailed("boom".into())),
            killed: RefCell::new(vec![]),
        };
        assert!(matches!(
            close(&facts, &c, CLAUDE, &id, &mut |_| {}),
            Err(LeftAlone::EnumerationFailed(_))
        ));
        assert!(facts.killed.borrow().is_empty());
    }
}
