// SPDX-License-Identifier: MIT OR Apache-2.0
//! The command line, parsed without side effects so it can be tested.

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Hook {
        event: String,
        reason: Option<String>,
    },
    List {
        json: bool,
        all: bool,
    },
    Reconcile {
        dry_run: bool,
        json: bool,
    },
    Pin {
        agent: String,
    },
    Unpin {
        agent: String,
    },
    /// `agent` is `None` for "the agent running this command".
    Waiting {
        agent: Option<String>,
        note: Option<String>,
        clear: bool,
    },
    Park {
        agent: String,
        confirm: Option<String>,
        yes: bool,
        timeout: Option<u64>,
    },
    Stop {
        agent: String,
        confirm: Option<String>,
        yes: bool,
        timeout: Option<u64>,
    },
    Unpark {
        agent: String,
        no_start: bool,
    },
    /// `flags` are `(config key, value)` pairs from the pacing flags, applied as the flag layer.
    Restore {
        dry_run: bool,
        json: bool,
        /// Run by the logon task: wait (bounded) for the network, the broker and the host program first.
        from_logon: bool,
        only: Option<Vec<String>>,
        priority: Vec<String>,
        flags: Vec<(String, String)>,
    },
    /// `agentlife pending`: the durable restores that wait for a person.
    Pending(PendingAction),
    /// `agentlife install-task`: print (or, with `register`, create) the logon and unlock tasks.
    InstallTask {
        register: bool,
        remove: bool,
        exe: Option<String>,
    },
    Version,
    Help,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PendingAction {
    List {
        json: bool,
        all: bool,
    },
    Show {
        id: String,
    },
    Approve {
        id: String,
    },
    Discard {
        id: String,
    },
    /// Run by the unlock task: if a restore waits, ask about it.
    Prompt,
}

pub const USAGE: &str = "\
agentlife: agent lifecycle control

USAGE:
    agentlife hook SessionStart
    agentlife hook SessionEnd --reason <clear|resume|logout|prompt_input_exit|other>
        The Claude Code hook entries. Reads the hook JSON on stdin and records the session in the
        registry. Never fails the session: it always exits 0, and writes what it did (or why it
        declined) to <home>/hook.log. Never run this by hand.

    agentlife list [--json] [--all]
        The registry joined with the process table. Closed (parked/exited) agents are hidden
        unless --all. Read-only.

    agentlife reconcile [--dry-run] [--json]
        For every agent that should be running but is not, say whether it was closed on purpose
        (a person ended its session before any shutdown) or killed. Writes down each
        \"closed on purpose\" verdict the first time it is derived; --dry-run writes nothing.

    agentlife park <agent> [--confirm <name>] [--yes] [--timeout <secs>]
    agentlife stop <agent> [--confirm <name>] [--yes] [--timeout <secs>]
    agentlife unpark <agent> --no-start
        Mark an agent as closed (parked is kept forever; stopped ages out), or clear the mark.
        A NOT-running agent is just marked. A RUNNING agent is stopped GRACEFULLY: it is asked, over
        claude-peers, to write its HANDOFF and run `lane-restart assert-idle`; agentlife waits (up to
        --timeout, default 600 s) until it provably has, then ends exactly that process. Without
        --yes it only prints what it would do. A lane cannot stop itself yet. A pinned agent (the PM,
        or one pinned by hand) needs --confirm with its name. <agent> is an id or a name.

    agentlife restore --dry-run [--json] [--only a,b] [--priority a,b] [--batch N] [--delay S]
                      [--max-running N] [--tabs-per-window N] [--free-ram-floor GB]
        Prints the plan for bringing back every agent that should be running and is not: the PM
        first and alone, then the --priority list, then the most recently active, in batches, in
        windows agentlife-<k>, within --max-running and the memory floor, with each agent's rebuilt
        launch arguments (anything not passed on is listed) and a hash of the whole plan. Writes
        nothing and starts nothing. Without --dry-run it refuses: starting agents needs a person's
        consent, which is not built yet.

    agentlife pending [list] [--json] [--all]
    agentlife pending show <id> | approve <id> | discard <id>
        The restores waiting for a person's answer. A pending restore is a record plus a frozen
        plan, not a process: nothing waits. `list` and `show` only read. `approve` asks the person
        (Windows Hello) about the frozen plan, and refuses if the registry has moved since, so the
        person never approves a plan that would no longer be the one executed. `discard` withdraws
        the request. Approving and discarding need the consent backend, which is not installed yet
        (OverMind's user-request is unpublished): they say so and change nothing.

    agentlife restore --from-logon
    agentlife pending --prompt
    agentlife install-task [--register | --remove] [--exe <path>]
        The two per-user Task Scheduler tasks (\"run only when the user is logged on\"): at logon,
        `restore --from-logon` first waits (up to network_wait_secs, default 120) for the network, the
        claude-peers broker and the lane-restart host program, then does what `restore` does; on
        session unlock, `pending --prompt` asks about a restore that is waiting, if there is one.
        `install-task` only PRINTS both tasks' XML and the schtasks commands; a person runs it with
        --register to create them (replacing same-named tasks) or --remove to delete them. Nothing
        starts an agent until the consent backend is installed.

    agentlife pin <agent> | unpin <agent>
        Never lazy-stop this agent. A lane may pin itself.

    agentlife waiting [--on user] [--note <text>] [--agent <agent>]
    agentlife waiting --clear [--agent <agent>]
        Mark (or unmark) that an agent is waiting for a person's answer, so it is not shut down
        meanwhile. With no --agent it is the agent running the command.

    agentlife --version | --help

STATE:
    <home> is $AGENTLIFE_HOME, else C:/Projects/.agentlife.
";

fn one_agent<'a>(rest: &'a [String], cmd: &str) -> Result<(&'a String, &'a [String]), String> {
    rest.split_first()
        .filter(|(a, _)| !a.starts_with('-'))
        .ok_or_else(|| format!("{cmd} needs an agent (an id or a name)"))
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let args: Vec<String> = args.into_iter().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(Command::Help);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Ok(Command::Version);
    }
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    match cmd.as_str() {
        "hook" => {
            let (event, rest) = rest
                .split_first()
                .ok_or("hook needs an event (SessionStart or SessionEnd)")?;
            if event != "SessionStart" && event != "SessionEnd" {
                return Err(format!(
                    "hook handles SessionStart and SessionEnd, not {event:?}"
                ));
            }
            let mut reason = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--reason" => {
                        reason = Some(it.next().ok_or("--reason needs a value")?.clone());
                    }
                    other => return Err(format!("unknown argument to hook: {other:?}")),
                }
            }
            Ok(Command::Hook {
                event: event.clone(),
                reason,
            })
        }
        "list" => {
            let (mut json, mut all) = (false, false);
            for a in rest {
                match a.as_str() {
                    "--json" => json = true,
                    "--all" => all = true,
                    other => return Err(format!("unknown argument to list: {other:?}")),
                }
            }
            Ok(Command::List { json, all })
        }
        "reconcile" => {
            let (mut dry_run, mut json) = (false, false);
            for a in rest {
                match a.as_str() {
                    "--dry-run" => dry_run = true,
                    "--json" => json = true,
                    other => return Err(format!("unknown argument to reconcile: {other:?}")),
                }
            }
            Ok(Command::Reconcile { dry_run, json })
        }
        "restore" => {
            let (mut dry_run, mut json, mut from_logon) = (false, false, false);
            let (mut only, mut priority) = (None, Vec::new());
            let mut flags = Vec::new();
            let list = |v: &str| -> Vec<String> {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut value = |flag: &str| -> Result<String, String> {
                    it.next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))
                };
                match a.as_str() {
                    "--dry-run" => dry_run = true,
                    "--json" => json = true,
                    "--from-logon" => from_logon = true,
                    "--only" => only = Some(list(&value("--only")?)),
                    "--priority" => priority = list(&value("--priority")?),
                    "--batch" => flags.push(("batch_size".to_string(), value("--batch")?)),
                    "--delay" => flags.push(("batch_delay_secs".to_string(), value("--delay")?)),
                    "--max-running" => {
                        flags.push(("max_running".to_string(), value("--max-running")?))
                    }
                    "--tabs-per-window" => {
                        flags.push(("tabs_per_window".to_string(), value("--tabs-per-window")?))
                    }
                    "--free-ram-floor" => {
                        flags.push(("free_ram_floor_gb".to_string(), value("--free-ram-floor")?))
                    }
                    other => return Err(format!("unknown argument to restore: {other:?}")),
                }
            }
            Ok(Command::Restore {
                dry_run,
                json,
                from_logon,
                only,
                priority,
                flags,
            })
        }
        "pending" if rest.len() == 1 && rest[0] == "--prompt" => {
            Ok(Command::Pending(PendingAction::Prompt))
        }
        "install-task" => {
            let (mut register, mut remove, mut exe) = (false, false, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--register" => register = true,
                    "--remove" => remove = true,
                    "--exe" => exe = Some(it.next().ok_or("--exe needs a path")?.clone()),
                    other => return Err(format!("unknown argument to install-task: {other:?}")),
                }
            }
            if register && remove {
                return Err("install-task: --register and --remove are exclusive".into());
            }
            Ok(Command::InstallTask {
                register,
                remove,
                exe,
            })
        }
        "pending" => {
            let (sub, tail) = match rest.split_first() {
                Some((s, t)) if !s.starts_with('-') => (s.as_str(), t),
                _ => ("list", rest),
            };
            let id_of = |tail: &[String]| -> Result<String, String> {
                match tail {
                    [id] if !id.starts_with('-') => Ok(id.clone()),
                    [] => Err(format!("pending {sub} needs a pending id")),
                    [_, extra, ..] => {
                        Err(format!("unexpected argument to pending {sub}: {extra:?}"))
                    }
                    [flag] => Err(format!("unknown argument to pending {sub}: {flag:?}")),
                }
            };
            Ok(Command::Pending(match sub {
                "list" => {
                    let (mut json, mut all) = (false, false);
                    for a in tail {
                        match a.as_str() {
                            "--json" => json = true,
                            "--all" => all = true,
                            other => {
                                return Err(format!("unknown argument to pending list: {other:?}"))
                            }
                        }
                    }
                    PendingAction::List { json, all }
                }
                "show" => PendingAction::Show { id: id_of(tail)? },
                "approve" => PendingAction::Approve { id: id_of(tail)? },
                "discard" => PendingAction::Discard { id: id_of(tail)? },
                other => return Err(format!("unknown pending subcommand: {other:?}")),
            }))
        }
        "pin" | "unpin" => {
            let (agent, tail) = one_agent(rest, cmd)?;
            if let Some(extra) = tail.first() {
                return Err(format!("unexpected argument to {cmd}: {extra:?}"));
            }
            Ok(if cmd == "pin" {
                Command::Pin {
                    agent: agent.clone(),
                }
            } else {
                Command::Unpin {
                    agent: agent.clone(),
                }
            })
        }
        "park" | "stop" => {
            let (agent, tail) = one_agent(rest, cmd)?;
            let mut confirm = None;
            let (mut yes, mut timeout) = (false, None);
            let mut it = tail.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--confirm" => {
                        confirm =
                            Some(it.next().ok_or("--confirm needs the agent's name")?.clone());
                    }
                    "--yes" => yes = true,
                    "--timeout" => {
                        let v = it.next().ok_or("--timeout needs a number of seconds")?;
                        let secs: u64 = v
                            .parse()
                            .map_err(|_| format!("--timeout wants whole seconds, not {v:?}"))?;
                        if secs == 0 {
                            return Err("--timeout must be at least 1 second".to_string());
                        }
                        timeout = Some(secs);
                    }
                    other => return Err(format!("unknown argument to {cmd}: {other:?}")),
                }
            }
            Ok(if cmd == "park" {
                Command::Park {
                    agent: agent.clone(),
                    confirm,
                    yes,
                    timeout,
                }
            } else {
                Command::Stop {
                    agent: agent.clone(),
                    confirm,
                    yes,
                    timeout,
                }
            })
        }
        "unpark" => {
            let (agent, tail) = one_agent(rest, cmd)?;
            let mut no_start = false;
            for a in tail {
                match a.as_str() {
                    "--no-start" => no_start = true,
                    other => return Err(format!("unknown argument to unpark: {other:?}")),
                }
            }
            if !no_start {
                return Err(
                    "unpark without --no-start would launch the agent, which needs the launcher and consent (not built yet); use --no-start to clear the mark only"
                        .to_string(),
                );
            }
            Ok(Command::Unpark {
                agent: agent.clone(),
                no_start,
            })
        }
        "waiting" => {
            let (mut agent, mut note, mut clear) = (None, None, false);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--on" => {
                        let on = it.next().ok_or("--on needs a value")?;
                        if on != "user" {
                            return Err(format!("--on supports only `user`, not {on:?}"));
                        }
                    }
                    "--note" => note = Some(it.next().ok_or("--note needs text")?.clone()),
                    "--agent" => agent = Some(it.next().ok_or("--agent needs an agent")?.clone()),
                    "--clear" => clear = true,
                    other => return Err(format!("unknown argument to waiting: {other:?}")),
                }
            }
            if clear && note.is_some() {
                return Err("--clear and --note do not go together".to_string());
            }
            Ok(Command::Waiting { agent, note, clear })
        }
        other => Err(format!("unknown command {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn hook_commands_parse_with_and_without_a_reason() {
        assert_eq!(
            p(&["hook", "SessionStart"]),
            Ok(Command::Hook {
                event: "SessionStart".into(),
                reason: None
            })
        );
        assert_eq!(
            p(&["hook", "SessionEnd", "--reason", "prompt_input_exit"]),
            Ok(Command::Hook {
                event: "SessionEnd".into(),
                reason: Some("prompt_input_exit".into())
            })
        );
    }

    #[test]
    fn hook_refuses_other_events_and_stray_arguments() {
        assert!(p(&["hook"]).is_err());
        assert!(p(&["hook", "PreToolUse"])
            .unwrap_err()
            .contains("PreToolUse"));
        assert!(p(&["hook", "SessionEnd", "--reason"]).is_err());
        assert!(p(&["hook", "SessionEnd", "--bogus"]).is_err());
    }

    #[test]
    fn list_flags_and_defaults() {
        assert_eq!(
            p(&["list"]),
            Ok(Command::List {
                json: false,
                all: false
            })
        );
        assert_eq!(
            p(&["list", "--all", "--json"]),
            Ok(Command::List {
                json: true,
                all: true
            })
        );
        assert!(p(&["list", "--wide"]).is_err());
    }

    #[test]
    fn pending_subcommands() {
        let list = |json, all| Ok(Command::Pending(PendingAction::List { json, all }));
        assert_eq!(p(&["pending"]), list(false, false));
        assert_eq!(p(&["pending", "list", "--all", "--json"]), list(true, true));
        assert_eq!(p(&["pending", "--json"]), list(true, false));
        assert_eq!(
            p(&["pending", "show", "p0001"]),
            Ok(Command::Pending(PendingAction::Show { id: "p0001".into() }))
        );
        assert_eq!(
            p(&["pending", "approve", "p1"]),
            Ok(Command::Pending(PendingAction::Approve { id: "p1".into() }))
        );
        assert_eq!(
            p(&["pending", "discard", "p1"]),
            Ok(Command::Pending(PendingAction::Discard { id: "p1".into() }))
        );
        assert!(p(&["pending", "approve"]).is_err(), "needs an id");
        assert!(p(&["pending", "approve", "--all"]).is_err());
        assert!(p(&["pending", "approve", "a", "b"]).is_err());
        assert!(p(&["pending", "list", "--wide"]).is_err());
        assert!(p(&["pending", "forget", "a"]).is_err());
    }

    #[test]
    fn restore_flags() {
        assert_eq!(
            p(&["restore", "--dry-run"]),
            Ok(Command::Restore {
                dry_run: true,
                json: false,
                from_logon: false,
                only: None,
                priority: vec![],
                flags: vec![]
            })
        );
        assert_eq!(
            p(&[
                "restore",
                "--dry-run",
                "--json",
                "--only",
                "a, b",
                "--priority",
                "x",
                "--batch",
                "5",
                "--delay",
                "30",
                "--max-running",
                "12",
                "--tabs-per-window",
                "4",
                "--free-ram-floor",
                "16"
            ]),
            Ok(Command::Restore {
                dry_run: true,
                json: true,
                from_logon: false,
                only: Some(vec!["a".into(), "b".into()]),
                priority: vec!["x".into()],
                flags: vec![
                    ("batch_size".into(), "5".into()),
                    ("batch_delay_secs".into(), "30".into()),
                    ("max_running".into(), "12".into()),
                    ("tabs_per_window".into(), "4".into()),
                    ("free_ram_floor_gb".into(), "16".into()),
                ]
            })
        );
        assert!(p(&["restore", "--batch"]).is_err(), "a flag with no value");
        assert!(p(&["restore", "--force"]).is_err());
    }

    #[test]
    fn logon_and_task_commands() {
        assert_eq!(
            p(&["restore", "--from-logon"]),
            Ok(Command::Restore {
                dry_run: false,
                json: false,
                from_logon: true,
                only: None,
                priority: vec![],
                flags: vec![]
            })
        );
        assert_eq!(
            p(&["pending", "--prompt"]),
            Ok(Command::Pending(PendingAction::Prompt))
        );
        assert!(p(&["pending", "--prompt", "--all"]).is_err());
        assert!(p(&["pending", "list", "--prompt"]).is_err());
        assert_eq!(
            p(&["install-task"]),
            Ok(Command::InstallTask {
                register: false,
                remove: false,
                exe: None
            })
        );
        assert_eq!(
            p(&["install-task", "--register", "--exe", "C:/a/agentlife.exe"]),
            Ok(Command::InstallTask {
                register: true,
                remove: false,
                exe: Some("C:/a/agentlife.exe".into())
            })
        );
        assert!(p(&["install-task", "--register", "--remove"]).is_err());
        assert!(p(&["install-task", "--exe"]).is_err());
        assert!(p(&["install-task", "--now"]).is_err());
    }

    #[test]
    fn reconcile_flags() {
        assert_eq!(
            p(&["reconcile"]),
            Ok(Command::Reconcile {
                dry_run: false,
                json: false
            })
        );
        assert_eq!(
            p(&["reconcile", "--json", "--dry-run"]),
            Ok(Command::Reconcile {
                dry_run: true,
                json: true
            })
        );
        assert!(p(&["reconcile", "--force"]).is_err());
    }

    #[test]
    fn pin_and_unpin_take_exactly_one_agent() {
        assert_eq!(
            p(&["pin", "synapse"]),
            Ok(Command::Pin {
                agent: "synapse".into()
            })
        );
        assert_eq!(
            p(&["unpin", "a-0123"]),
            Ok(Command::Unpin {
                agent: "a-0123".into()
            })
        );
        assert!(p(&["pin"]).is_err());
        assert!(p(&["pin", "--all"]).is_err(), "a flag is not an agent");
        assert!(p(&["pin", "a", "b"]).is_err());
    }

    #[test]
    fn park_and_stop_take_an_agent_and_an_optional_confirmation() {
        assert_eq!(
            p(&["park", "lane"]),
            Ok(Command::Park {
                agent: "lane".into(),
                confirm: None,
                yes: false,
                timeout: None
            })
        );
        assert_eq!(
            p(&["stop", "PM", "--confirm", "PM"]),
            Ok(Command::Stop {
                agent: "PM".into(),
                confirm: Some("PM".into()),
                yes: false,
                timeout: None
            })
        );
        assert!(p(&["park"]).is_err());
        assert!(p(&["park", "lane", "--confirm"]).is_err());
        assert!(p(&["park", "lane", "--force"]).is_err());
    }

    #[test]
    fn park_and_stop_take_yes_and_a_timeout_for_a_running_agent() {
        assert_eq!(
            p(&["park", "lane", "--yes", "--timeout", "90"]),
            Ok(Command::Park {
                agent: "lane".into(),
                confirm: None,
                yes: true,
                timeout: Some(90)
            })
        );
        assert!(p(&["stop", "lane", "--timeout"]).is_err());
        assert!(p(&["stop", "lane", "--timeout", "soon"]).is_err());
        assert!(p(&["stop", "lane", "--timeout", "0"]).is_err());
    }

    #[test]
    fn unpark_requires_no_start_because_starting_is_not_built() {
        assert_eq!(
            p(&["unpark", "lane", "--no-start"]),
            Ok(Command::Unpark {
                agent: "lane".into(),
                no_start: true
            })
        );
        let e = p(&["unpark", "lane"]).unwrap_err();
        assert!(e.contains("--no-start") && e.contains("launch"), "{e}");
    }

    #[test]
    fn waiting_defaults_to_the_calling_agent_and_validates_its_flags() {
        assert_eq!(
            p(&["waiting"]),
            Ok(Command::Waiting {
                agent: None,
                note: None,
                clear: false
            })
        );
        assert_eq!(
            p(&[
                "waiting",
                "--on",
                "user",
                "--note",
                "needs your OK",
                "--agent",
                "x"
            ]),
            Ok(Command::Waiting {
                agent: Some("x".into()),
                note: Some("needs your OK".into()),
                clear: false
            })
        );
        assert_eq!(
            p(&["waiting", "--clear"]),
            Ok(Command::Waiting {
                agent: None,
                note: None,
                clear: true
            })
        );
        assert!(p(&["waiting", "--on", "peer"]).is_err());
        assert!(p(&["waiting", "--clear", "--note", "x"]).is_err());
        assert!(p(&["waiting", "--note"]).is_err());
    }

    #[test]
    fn help_version_and_unknown() {
        assert_eq!(p(&[]), Ok(Command::Help));
        assert_eq!(p(&["--help"]), Ok(Command::Help));
        assert_eq!(p(&["list", "-h"]), Ok(Command::Help));
        assert_eq!(p(&["--version"]), Ok(Command::Version));
        assert!(p(&["launch-everything"])
            .unwrap_err()
            .contains("launch-everything"));
    }

    #[test]
    fn the_usage_text_documents_every_command() {
        for word in [
            "hook SessionStart",
            "hook SessionEnd",
            "list",
            "reconcile",
            "restore --dry-run",
            "pending",
            "install-task",
            "--from-logon",
            "pending --prompt",
            "park",
            "stop",
            "unpark",
            "pin",
            "unpin",
            "waiting",
            "hook.log",
            "AGENTLIFE_HOME",
        ] {
            assert!(USAGE.contains(word), "{word}");
        }
    }
}
