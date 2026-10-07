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
    Version,
    Help,
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

    agentlife --version | --help

STATE:
    <home> is $AGENTLIFE_HOME, else C:/Projects/.agentlife.
";

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
    fn help_version_and_unknown() {
        assert_eq!(p(&[]), Ok(Command::Help));
        assert_eq!(p(&["--help"]), Ok(Command::Help));
        assert_eq!(p(&["list", "-h"]), Ok(Command::Help));
        assert_eq!(p(&["--version"]), Ok(Command::Version));
        assert!(p(&["restore"]).unwrap_err().contains("restore"));
    }

    #[test]
    fn the_usage_text_documents_every_command() {
        for word in [
            "hook SessionStart",
            "hook SessionEnd",
            "list",
            "hook.log",
            "AGENTLIFE_HOME",
        ] {
            assert!(USAGE.contains(word), "{word}");
        }
    }
}
