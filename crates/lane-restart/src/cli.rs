// SPDX-License-Identifier: MIT OR Apache-2.0

//! clap-derive argument grammars for the cold subcommands. Each keeps the hand-written parser's
//! behaviour: the same spellings, a flag's value is taken verbatim even when it starts with `-`
//! (`allow_hyphen_values`), a repeated flag is last-wins unless the old parser refused it, and
//! `--help`/`--version` stay main's (clap's are disabled). A failed parse is the caller's exit 1.

use clap::Parser;
use std::path::PathBuf;

/// One subcommand's tail, parsed as if `lane-restart <name>` had been typed. `takes_value` names
/// the flags that consume the next token whatever it is: a bare `--` anywhere else would be clap's
/// end-of-options marker, which the hand-written parsers never had - it is refused as unknown.
fn parse<T: Parser>(name: &str, takes_value: &[&str], rest: &[String]) -> Result<T, String> {
    let mut prev_takes_value = false;
    for a in rest {
        if a == "--" && !prev_takes_value {
            return Err("unexpected argument \"--\"".to_string());
        }
        prev_takes_value = !prev_takes_value && takes_value.contains(&a.as_str());
    }
    T::try_parse_from(std::iter::once(format!("lane-restart {name}")).chain(rest.iter().cloned()))
        .map_err(|e| e.render().to_string().trim_end().to_string())
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Install {
    #[arg(long, allow_hyphen_values = true, overrides_with = "from")]
    from: PathBuf,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
}

/// `install --from <exe> [--dry-run]` -> `(exe, dry_run)`.
pub fn install(rest: &[String]) -> Result<(PathBuf, bool), String> {
    let a: Install = parse("install", &["--from"], rest)?;
    Ok((a.from, a.dry_run))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Status {
    #[arg(long, overrides_with = "json")]
    json: bool,
    #[arg(long, overrides_with = "peers")]
    peers: bool,
    /// Positive whole seconds; given twice is an error (the default clap behaviour).
    #[arg(long, allow_hyphen_values = true, value_parser = positive_seconds)]
    quiet_over: Option<i64>,
}

fn positive_seconds(v: &str) -> Result<i64, String> {
    v.parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("not a positive whole number: {v:?}"))
}

/// `status [--json] [--quiet-over <secs>] [--peers]` -> `(json, quiet_over, peers)`.
pub fn status(rest: &[String]) -> Result<(bool, Option<i64>, bool), String> {
    let a: Status = parse("status", &["--quiet-over"], rest)?;
    Ok((a.json, a.quiet_over, a.peers))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Ledger {
    #[arg(long, allow_hyphen_values = true, overrides_with = "role")]
    role: Option<String>,
    #[arg(long, allow_hyphen_values = true, overrides_with = "kind")]
    kind: Option<String>,
    #[arg(long, allow_hyphen_values = true, overrides_with = "since")]
    since: Option<String>,
    #[arg(long, overrides_with_all = ["acted", "refused"])]
    acted: bool,
    #[arg(long, overrides_with_all = ["acted", "refused"])]
    refused: bool,
    #[arg(long, allow_hyphen_values = true, overrides_with = "last")]
    last: Option<usize>,
    #[arg(long, overrides_with = "json")]
    json: bool,
    #[arg(long, overrides_with = "by_lane")]
    by_lane: bool,
}

/// `ledger [--role R] [--kind K] [--since T] [--acted|--refused] [--last N] [--json] [--by-lane]`
/// -> `(filter, json, since as typed, by_lane)`.
pub fn ledger(
    rest: &[String],
) -> Result<(lane_restart::ledger::Filter, bool, Option<String>, bool), String> {
    let a: Ledger = parse("ledger", &["--role", "--kind", "--since", "--last"], rest)?;
    let acted = match (a.acted, a.refused) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let f = lane_restart::ledger::Filter {
        role: a.role,
        kind: a.kind,
        acted,
        last: a.last,
        ..Default::default()
    };
    Ok((f, a.json, a.since, a.by_lane))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct AssertIdle {
    #[arg(long, allow_hyphen_values = true, overrides_with = "role")]
    role: Option<String>,
}

/// `assert-idle [--role <name>]` and `approvals [--role <name>]` -> the role.
pub fn assert_idle(rest: &[String]) -> Result<Option<String>, String> {
    let a: AssertIdle = parse("assert-idle", &["--role"], rest)?;
    Ok(a.role)
}

/// `waits [--role <name>]` -> the role.
pub fn waits(rest: &[String]) -> Result<Option<String>, String> {
    let a: AssertIdle = parse("waits", &["--role"], rest)?;
    Ok(a.role)
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct SpecsImport {
    #[arg(long, allow_hyphen_values = true, overrides_with = "role")]
    role: Option<String>,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
    #[arg(long, overrides_with = "verbose")]
    verbose: bool,
}

/// `specs import [--role R] [--dry-run] [--verbose]` -> `(role, dry_run, verbose)`.
pub fn specs_import(rest: &[String]) -> Result<(Option<String>, bool, bool), String> {
    let a: SpecsImport = parse("specs import", &["--role"], rest)?;
    Ok((a.role, a.dry_run, a.verbose))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct SpecsRetire {
    #[arg(long = "role", allow_hyphen_values = true, required = true)]
    roles: Vec<String>,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
}

/// `specs retire --role R [--role R]... [--dry-run]` -> `(roles, dry_run)`.
pub fn specs_retire(rest: &[String]) -> Result<(Vec<String>, bool), String> {
    let a: SpecsRetire = parse("specs retire", &["--role"], rest)?;
    Ok((a.roles, a.dry_run))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct SpecsSynapseOnly {
    #[arg(long = "role", allow_hyphen_values = true, required = true)]
    roles: Vec<String>,
    #[arg(long, overrides_with = "strict")]
    strict: bool,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
}

/// `specs synapse-only --role R [--role R]... [--strict] [--dry-run]` -> `(roles, strict, dry_run)`.
pub fn specs_synapse_only(rest: &[String]) -> Result<(Vec<String>, bool, bool), String> {
    let a: SpecsSynapseOnly = parse("specs synapse-only", &["--role"], rest)?;
    Ok((a.roles, a.strict, a.dry_run))
}

/// `specs both-channels --role R [--role R]... [--dry-run]` -> `(roles, dry_run)`.
pub fn specs_both_channels(rest: &[String]) -> Result<(Vec<String>, bool), String> {
    let a: SpecsRetire = parse("specs both-channels", &["--role"], rest)?;
    Ok((a.roles, a.dry_run))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct LaneNew {
    role: String,
    repo: String,
    #[arg(long, allow_hyphen_values = true, overrides_with = "model")]
    model: Option<String>,
    #[arg(long, overrides_with = "apply")]
    apply: bool,
    #[arg(long, allow_hyphen_values = true, overrides_with = "plan")]
    plan: Option<String>,
}

/// `(role, repo, model, apply, plan)` of a `lane-new` command line.
pub type LaneNewArgs = (String, String, String, bool, Option<String>);

/// `lane-new <role> <repo> [--model M] [--apply --plan H]`; the model defaults to `sonnet`.
pub fn lane_new(rest: &[String]) -> Result<LaneNewArgs, String> {
    let a: LaneNew = parse("lane-new", &["--model", "--plan"], rest)?;
    Ok((
        a.role,
        a.repo,
        a.model.unwrap_or_else(|| "sonnet".to_string()),
        a.apply,
        a.plan,
    ))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Start {
    #[arg(
        long,
        allow_hyphen_values = true,
        required = true,
        overrides_with = "role"
    )]
    role: String,
    #[arg(long, allow_hyphen_values = true, overrides_with = "reason")]
    reason: Option<String>,
    #[arg(long, allow_hyphen_values = true, overrides_with = "model")]
    model: Option<String>,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
    #[arg(long, overrides_with = "json")]
    json: bool,
    #[arg(long, allow_hyphen_values = true, overrides_with = "signal_id")]
    signal_id: Option<String>,
}

/// `start --role R [--reason <text>] [--model M] [--signal-id I] [--dry-run] [--json]`. A missing
/// `--reason` is an empty one: `start::validate_reason` owns that refusal.
pub fn start(rest: &[String]) -> Result<crate::StartArgs, String> {
    let takes = ["--role", "--reason", "--model", "--signal-id"];
    let a: Start = parse("start", &takes, rest)?;
    Ok(crate::StartArgs {
        role: a.role,
        reason: a.reason.unwrap_or_default(),
        model: a.model,
        dry_run: a.dry_run,
        json: a.json,
        signal_id: a.signal_id,
    })
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Stop {
    #[arg(
        long,
        allow_hyphen_values = true,
        required = true,
        overrides_with = "role"
    )]
    role: String,
    #[arg(long, allow_hyphen_values = true, overrides_with = "reason")]
    reason: Option<String>,
    #[arg(long = "self", overrides_with = "target_self")]
    target_self: bool,
    #[arg(long, overrides_with = "soft", conflicts_with = "hard")]
    soft: bool,
    #[arg(long, overrides_with = "hard", conflicts_with = "soft")]
    hard: bool,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
    #[arg(long = "yes", overrides_with = "confirmed")]
    confirmed: bool,
}

/// `stop --role R [--reason <text>] [--self] [--soft|--hard] [--dry-run] [--yes]`.
pub fn stop(rest: &[String]) -> Result<crate::StopArgs, String> {
    let a: Stop = parse("stop", &["--role", "--reason"], rest)?;
    let kind = match (a.soft, a.hard) {
        (true, _) => Some(lane_restart::spec::StopKind::Soft),
        (_, true) => Some(lane_restart::spec::StopKind::Hard),
        _ => None,
    };
    Ok(crate::StopArgs {
        role: a.role,
        reason: a.reason.unwrap_or_default(),
        target_self: a.target_self,
        dry_run: a.dry_run,
        confirmed: a.confirmed,
        kind,
    })
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Attach {
    #[arg(long, allow_hyphen_values = true, overrides_with = "role")]
    role: Option<String>,
    #[arg(long, overrides_with = "watch")]
    watch: bool,
}

/// `attach --role <r> --watch` -> `(role, watch)`. Whether the role is an identifier and whether
/// `--watch` was given are the caller's refusals, each with its own message.
pub fn attach(rest: &[String]) -> Result<(Option<String>, bool), String> {
    let a: Attach = parse("attach", &["--role"], rest)?;
    Ok((a.role, a.watch))
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Restart {
    #[arg(
        long,
        allow_hyphen_values = true,
        required_unless_present = "target_self",
        overrides_with = "role"
    )]
    role: Option<String>,
    #[arg(long = "self", overrides_with = "target_self")]
    target_self: bool,
    #[arg(long, overrides_with = "dry_run")]
    dry_run: bool,
    #[arg(long = "yes", overrides_with = "confirmed")]
    confirmed: bool,
}

/// The restart flags: `(--role <name> | --self) [--dry-run] [--yes]`.
///
/// ⚠️ `--self` DEFAULTS TO FALSE. Omitting it means "restart a different
/// lane" - the SAFER default is the one that requires `--yes` to act for
/// real, not the one that acts unconditionally. A caller who wants to
/// restart their own session must say so explicitly.
pub fn restart(rest: &[String]) -> Result<crate::Args, String> {
    let a: Restart = parse("", &["--role"], rest)?;
    Ok(crate::Args {
        role: a.role,
        target_self: a.target_self,
        dry_run: a.dry_run,
        confirmed: a.confirmed,
    })
}

#[derive(Parser, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct Host {
    #[arg(
        long,
        allow_hyphen_values = true,
        required = true,
        overrides_with = "role"
    )]
    role: String,
    /// Everything after the first `--` that is not a `--role` value, verbatim.
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    child: Vec<String>,
}

/// `host --role <role> -- <child argv...>` -> `(role, child_argv)`. The `--` separator belongs to
/// this grammar, so `parse`'s refusal of a bare `--` does not apply.
pub fn host(rest: &[String]) -> Result<(String, Vec<String>), String> {
    let a = Host::try_parse_from(
        std::iter::once("lane-restart host".to_string()).chain(rest.iter().cloned()),
    )
    .map_err(|e| e.render().to_string().trim_end().to_string())?;
    Ok((a.role, a.child))
}

#[cfg(test)]
mod lane_new_tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lane_new_takes_a_plan_hash_and_defaults_to_a_dry_run_without_one() {
        let (role, repo, model, apply, plan) = lane_new(&args(&[
            "rockfish",
            "rockfish",
            "--apply",
            "--plan",
            "0123456789abcdef",
        ]))
        .unwrap();
        assert_eq!(
            (role.as_str(), repo.as_str(), model.as_str()),
            ("rockfish", "rockfish", "sonnet")
        );
        assert!(apply);
        assert_eq!(plan.as_deref(), Some("0123456789abcdef"));
        // control: no flags is a dry run with no plan
        let (_, _, _, apply, plan) = lane_new(&args(&["rockfish", "rockfish"])).unwrap();
        assert!(!apply && plan.is_none());
    }
}
