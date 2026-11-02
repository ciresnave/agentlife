// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `lane-restart` command line: every subcommand's glue. It was `main.rs`; the binary is now a
//! wrapper around [`run`] (CireSnave 2026-11-01: "any Rust binary should be a wrapper around its
//! library so that the library can be used for testing all internal functionality"), reached as
//! `lane_restart::cli::run`. All the actual decision logic lives in the other modules
//! (`authorize.rs`, `facts.rs`, `state.rs`, `log.rs`) so it can be unit tested without a real
//! process. This file only: parses argv, wires the real `SystemFacts`, calls `authorize::decide`,
//! and - only if it comes back `Ok` with `will_act: true` - performs the kill and the relaunch.

mod approvals_cmd;
mod assert_idle_cmd;
mod attach_cmd;
mod ctx;
mod install_cmd;
mod lane_new_cmd;
mod ledger_cmd;
mod specs_cmd;
mod start_cmd;
mod status_cmd;
mod stop_cmd;
#[cfg(test)]
mod test_fixtures;
mod waits_cmd;
mod waits_poll_cmd;

use crate::cli;
use ctx::Ctx;
use lane_restart::authorize::{self, Target};
use lane_restart::facts::SysinfoFacts;
use lane_restart::lane_state_writer::{self, RealParentProcess};
use lane_restart::lane_stop;
use lane_restart::log;
use lane_restart::relaunch;
use lane_restart::spec::StopKind;
use lane_restart::start;
use lane_restart::tab_close;
use std::path::PathBuf;
use std::process::ExitCode;

/// `C:/Projects/.lane-state` - RESTART-TOOL-DESIGN.md §7. Not configurable
/// via CLI on purpose: a caller-supplied state directory would defeat the
/// whole point of a fixed, portfolio-wide location every lane and the PM
/// agree on.
fn state_dir() -> PathBuf {
    lane_restart::state_dir()
}

fn log_path() -> PathBuf {
    state_dir().join("restart.log")
}

fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".claude")))
        .expect("could not resolve the Claude Code config directory")
}

fn dirs_home() -> Option<PathBuf> {
    lane_restart::paths::home_dir()
}

#[derive(Debug)]
pub(crate) struct Args {
    /// `None` only with `--self` and no `--role`: resolved in `main` before use.
    pub(crate) role: Option<String>,
    pub(crate) target_self: bool,
    pub(crate) dry_run: bool,
    pub(crate) confirmed: bool,
}

/// `lane-restart state <event>` - the hook command. Handled separately from
/// `cli::restart` because its shape (a positional subcommand, then a single
/// positional event name) doesn't fit the restart CLI's own flags at all,
/// and mixing the two would make either grammar harder to read.
/// It stays a slice pattern in `main` (`[cmd, event]`): there is no flag loop to replace, and it is
/// the hook hot path.
fn run_state_hook(event: &str) -> ExitCode {
    let started = std::time::Instant::now();
    let my_pid = std::process::id();
    let lane_role_env = std::env::var("LANE_ROLE").ok();
    let mut stdin = std::io::stdin();
    let lookup = RealParentProcess::new();
    let result = lane_state_writer::run(
        &state_dir(),
        event,
        my_pid,
        &lookup,
        lane_role_env.as_deref(),
        &mut stdin,
    );
    if let Some(note) = lane_state_writer::slow_note(event, started.elapsed(), lookup.full_scans())
    {
        eprintln!("{note}");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lane-restart state {event}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The text of `role`'s HANDOFF under `cwd`, if it has one and it is readable.
fn handoff_text(cwd: &str, role: &str) -> Option<String> {
    let (path, _) = lane_restart::spec::find_handoff(cwd, role)?;
    std::fs::read_to_string(path).ok()
}

/// `lane-restart host --role <role> -- <child argv...>` - RESTART-TOOL-
/// DESIGN.md §12.5. Runs INSIDE the relaunched session's own terminal tab
/// (launched by `spawn_relaunch`, below) and hosts `claude` in a ConPTY
/// this process owns.
fn run_host_cmd(role: &str, child_argv: &[String]) -> ExitCode {
    match lane_restart::host::run(role, child_argv) {
        Ok(outcome) => match outcome.child_exit_code {
            Some(0) | None => ExitCode::SUCCESS,
            Some(_) => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("lane-restart host: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `lane-restart --version` - RESTART-TOOL-DESIGN.md §12.9: the crate
/// version, that it embeds no approvals, and where it would fetch them
/// from. No network: `lane-restart approvals` does the fetch.
fn print_version() {
    use lane_restart::approvals;
    println!("lane-restart {}", env!("CARGO_PKG_VERSION"));
    println!("embedded approvals: none (by design - RESTART-TOOL-DESIGN.md §12.2)");
    match dirs_home() {
        Some(home) => match approvals::load_config(&approvals::config_path(&home)) {
            Ok(c) => println!(
                "approvals source: {}:{} (default branch), notify role: {}",
                c.approvals.repo, c.approvals.path, c.notify_role
            ),
            Err(e) => println!("approvals source: {e}"),
        },
        None => println!("approvals source: no home directory"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StartArgs {
    pub(crate) role: String,
    pub(crate) reason: String,
    pub(crate) model: Option<String>,
    pub(crate) dry_run: bool,
    pub(crate) json: bool,
    pub(crate) signal_id: Option<String>,
}

/// The disk floor (`disk.rs`), checked before any launch. A refusal is printed as `kind: refused -
/// ...` and the caller exits 1 and logs it; an unreadable probe is a warning, not a refusal.
fn disk_gate(kind: &str) -> Result<(), lane_restart::disk::Refusal> {
    match lane_restart::disk::guard(&state_dir()) {
        Ok(lane_restart::disk::GuardOk::Free(_)) => Ok(()),
        Ok(lane_restart::disk::GuardOk::Unknown(why)) => {
            eprintln!("lane-restart {kind}: warning - free disk space was not checked: {why}");
            Ok(())
        }
        Err(refusal) => {
            eprintln!("lane-restart {kind}: refused - {refusal}");
            Err(refusal)
        }
    }
}

/// The role whose `claude` session is running this command, verified by pid and start time
/// (RESTART-TOOL-DESIGN.md 14.8), never by `LANE_ROLE`.
fn verified_caller(facts: &SysinfoFacts) -> Result<String, start::CallerError> {
    let pid =
        lane_state_writer::claude_parent_pid(std::process::id(), &RealParentProcess::new()).ok();
    start::caller_role(&state_dir(), pid, facts)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StopArgs {
    pub(crate) role: String,
    pub(crate) reason: String,
    pub(crate) target_self: bool,
    pub(crate) dry_run: bool,
    pub(crate) confirmed: bool,
    /// `--soft` / `--hard`; `None` takes the default (soft for `--self`, hard otherwise).
    pub(crate) kind: Option<StopKind>,
}

const USAGE: &str = "\
lane-restart - a bulletproof session-restart tool. RESTART-TOOL-DESIGN.md.

USAGE:
    lane-restart (--role <name> | --self) [--dry-run] [--yes]
        Restart a lane. --self restarts the CALLING lane (no idle check) and
        needs no --role: the role is LANE_ROLE, else the one role whose lane
        state records the current directory (--role overrides);
        omitting it targets a DIFFERENT lane, which must be independently
        idle and requires --yes to act for real - otherwise it prints a dry
        run and does nothing.
        Exit codes (PM finding, 2026-09-19: distinct codes so scripts can
        tell these apart): 0 = relaunched with real progress confirmed;
        2 = killed and relaunched, but awaiting a human at the dev-channels
        confirmation dialog; 1 = refused, or the relaunch failed outright.

    lane-restart state <event>
        The hook subcommand: reads a hook's JSON input on stdin and updates
        .lane-state/<role>.json. Wired into settings.json; never run this by
        hand.

    lane-restart assert-idle [--role <name>]
        Run by a lane ITSELF, from its own shell, right after writing
        HANDOFF: asserts that no background shell it started is still
        running. Also WARNS (exit code unchanged) when the HANDOFF is over
        its size ceiling: 8 KB for a lane, 20 KB for the PM
        (CIRESNAVE-EXPECTATIONS.md 2.3c). Required before any restart of that lane can be
        authorized - RESTART-TOOL-DESIGN.md §1a. Role defaults to
        LANE_ROLE, then falls back to the current directory's leaf name,
        the same as the state hook. REFUSES (exit 1, nothing recorded) a
        HANDOFF with a malformed `WAIT:` line; see `waits`.

    lane-restart waits [--role <name>]
        Waits that cannot be forgotten. A lane that waits for something puts one
        line per wait in its HANDOFF:
            WAIT: <event> | check: <command> | wake: <RFC3339 UTC time>
        (absolute time; no `check` or no `wake` is refused by assert-idle). This
        prints each wait soonest first with the time left; a starting lane runs
        it and arms a timer (ScheduleWakeup or a cron) for every `wake`, because a
        session timer dies with its session. Exit 1 when a line is malformed.

    lane-restart waits-poll [--dry-run]
        The supervisor's pass over those WAIT lines: for every live role with a
        complete spec (never the PM) it reads the HANDOFF and tells the lane, once
        per (role, event, wake), when a wait is due: the check passed, the wake time
        came, or the check is not one the supervisor will run. It NEVER runs a
        lane-written command: only `gh pr view <N> --json state,mergedAt`,
        `gh run view <N> --json conclusion,status` and `git ls-remote --heads
        <remote> <branch>` (argv exec, no shell, 30 s timeout); anything else is
        reported 'unverified, look yourself'. One pass, no daemon: call it from a
        periodic job. The notice goes out on the configured notify_transport.
        --dry-run sends and records nothing.

    lane-restart host --role <name> -- <argv...>
        Internal: runs inside the relaunched session's own terminal tab
        (RESTART-TOOL-DESIGN.md §12.5). Loads the user's approvals, owns a
        ConPTY, hosts <argv...> (normally `claude ...`) inside it, relays
        every byte transparently, and - only during the startup window -
        checks the screen against every approval (RESTART-TOOL-DESIGN.md
        §12), injecting the matching one's action. If the window ends on a
        dialog nothing answered, it messages the notify role's lane.
        Wired automatically by a restart; never run this by hand.

    lane-restart approvals [--role <name>]
        Fetch the approvals configured in ~/.overmind/lane-restart.json
        (from that repo's default branch only) and list every active and
        refused one, exactly as `host` would load them.

    lane-restart status [--json] [--quiet-over <secs>] [--peers]
        --quiet-over marks (QUIET>Ns; activity.quiet_over in JSON) each live
        role whose last hook write is older than <secs>. Opt-in, nothing is
        enforced and there is no default; the PM's recommended value is 120.
        Read-only: each role's state (running, suspended, crashed, failed_relaunch, never_run,
        unknown), whether its launch spec (<role>.launch.json, written before a
        restart's kill and at a confirmed launch) is complete, and whether it can
        be started again from it. RESTART-TOOL-DESIGN.md section 14.
        --peers adds, per role, the claude-peers sessions whose helper's PARENT is that
        role's live claude (never joined by cwd), the sessions no role claims, and a
        truncated summary. Read-only; a broker that cannot be read is
        unknown for every role, never absent.

    lane-restart attach --role <name> --watch
        Read-only: shows the screen of a running direct lane host and then its live
        output, until the host ends or you press Ctrl-C. Nothing you type reaches the
        lane. --watch is required; interactive attach does not exist. Linux/unix only.

    lane-restart start --role <name> --reason <text> [--model sonnet|haiku] [--dry-run]
        Start a lane that is not running, from its launch spec
        (<role>.launch.json), and write the spec back as running once the
        session is confirmed. PM ONLY: the caller's claude session must be
        the pid and start time recorded by pm's state file (a lane cannot
        claim it with LANE_ROLE; a lane that edits pm.json can forge it -
        this closes the accidental and prompt-injected paths, not a hostile
        local user). --reason is required (<= 200 chars, no control
        characters) and goes to restart.log, not into the prompt. The model
        is sonnet or haiku (default: the spec's, else sonnet); Opus is
        refused and LANE_RESTART_ALLOW_OPUS is not read. The permission mode
        is always auto. Starting role pm is refused: it is started by a
        person. Exit codes: 0 started with progress confirmed, 2 started and
        awaiting the dialog (--json: one outcome line on stdout), 3 the lane is already running (nothing was
        launched: a no-op, safe to call again), 1 any other refusal or a
        failed launch.
        REFUSED (duplicate-risk) while the process table holds a
        `lane-restart host --role <name>` or a claude named for the role,
        whatever the state file says; the pids are printed. A restart
        refuses the same way for any such process other than its own
        session and that session's host. `status` marks such roles
        duplicate-risk.

    lane-restart stop --role <name> --reason <text> [--self] [--soft|--hard] [--dry-run] [--yes]
        Suspend a lane. The complete launch spec (<role>.launch.json) is
        written durably and read back BEFORE the process is touched, then the
        verified kill, then the spec is marked suspended: a lane is stopped
        only if it can be started again. Needs a HANDOFF written during the
        lane's session (and not after it asserted idle); there is no
        override. Another lane: PM only (same process-ancestry check as
        start), idle, and --yes to act for real, otherwise a dry run. --self
        stops the calling lane. Role pm is refused. --reason as for start.
        The stop is recorded in the spec as soft or hard (stop_kind): HARD (the default) is
        a deliberate stop that no launcher may undo; SOFT is a lane ending in the course of
        its own work, which a message for it may start again. --self is always soft and
        refuses --hard; a lane can never turn a hard stop into a soft one. A spec with no
        kind (older, or a crash import) is unclassified and is never launcher-started.
        Exit codes: 0 stopped and suspended; 1 refused or failed; 4 stopped,
        but the suspended mark could not be written (still startable).

    lane-restart specs import [--role <name>] [--dry-run] [--verbose]
        PM only. The first and last lines say whether anything was written
        (DRY RUN - nothing written / WROTE n spec(s)). Refused roles are all
        listed; roles refused for the same reason (e.g. cwd missing) share one
        line with a count unless --verbose. For every lane state file with no launch spec, build
        <role>.launch.json from its recorded launch_args, labelled
        written_by=import, only if the spec is complete; the rest are listed
        with the reason. Never overwrites a spec; never edits a state file.

    lane-restart specs retire --role <name> [--role <name>]... [--dry-run]
        PM only. For each role whose recorded cwd is not a directory and whose
        recorded process is not live, MOVE its state file (and launch spec, if
        any) to <state dir>/retired/<file>.<UTC stamp>; never deletes, never
        overwrites, reads the move back. Role pm, a role with a directory
        (resume it with `start`) and a role with a live process are refused
        with the reason. First and last lines say whether anything moved.

    lane-restart lane-new <role> <repo> [--model sonnet|haiku] [--apply --plan <hash>]
        PM only (docs/LANE-NEW.md). Dry run (default): validates the names, the disk and
        what exists (reading GitHub: repo, main, files), prints every step with its
        absolute paths, writes and runs nothing, and ends with `plan <hash>`.
        --apply creates the public repo, seeds it, writes the lane files, provisions the
        Synapse identity and starts the lane; it is refused unless --plan is the hash of
        a dry run of exactly the same role, repo and model. An existing repo is used only
        if empty or holding nothing but the seed.

    lane-restart specs both-channels --role <name> [--role <name>]... [--dry-run]
        Writing is PM only; --dry-run writes nothing and any caller may run it. Bring a
        role's launch_args to the full push-channel form: --mcp-config <role>.mcp.json
        and --dangerously-load-development-channels server:claude-peers server:synapse
        (other channels are kept). Refused, untouched: the config file missing or without
        a synapse server, or --strict-mcp-config in the spec. The old spec is copied once
        to <role>.launch.json.pre-both-channels. A running session is not touched: it
        takes the change at its next launch. `status` shows each running lane's flags.

    lane-restart specs synapse-only --role <name> [--role <name>]... [--strict] [--dry-run]
        PM only (docs/SYNAPSE-ONLY-CUTOVER.md). Rewrite a role's launch_args so its
        NEXT launch loads server:synapse and not server:claude-peers; --strict also
        adds --strict-mcp-config so the user-scope claude-peers server stays out of
        that session. Refused, untouched: no --mcp-config in the spec, that file
        missing or without a synapse server, or no dev-channels flag. The old spec
        is copied once to <role>.launch.json.pre-synapse and never overwritten (roll
        back by copying it over the spec). A running session is not touched.

    lane-restart install --from <exe> [--dry-run]
        PM only. Replace C:/Projects/.claude-hooks/lane-restart.exe with a
        freshly built one. The candidate must run `--version` (first line
        `lane-restart x.y.z`) and `status` (exit 0) BEFORE anything changes.
        The installed exe is renamed to lane-restart.exe.bak (the one .bak;
        the previous .bak is replaced), the candidate is copied in and read
        back, and lane-restart.sha256 is rewritten and read back. A failure
        after the rename puts the old exe back. --dry-run verifies and
        reports, writes nothing.

    lane-restart ledger [--role <name>] [--kind <kind>] [--since <time|30m|2h|1d>]
                        [--acted | --refused] [--last <n>] [--json | --by-lane]
        Read-only: prints restart.log, oldest first: time, role, kind
        (restart if the line predates kinds), who asked (and the verified
        caller if it differs), acted or refused, and the outcome. A line that
        is not an entry is counted and named in the footer, never skipped
        silently. Writes nothing.

    lane-restart models
        Read-only: list each lane's recorded model and flag Opus. A relaunch
        pins `--model` from the original launch's explicit --model, else
        `model-policy.json` (default sonnet) - never the recorded model - and
        refuses Opus unless LANE_RESTART_ALLOW_OPUS=1.

    lane-restart --version
        Print the crate version and the configured approvals source. This
        binary embeds no approvals (RESTART-TOOL-DESIGN.md §12.2).

    lane-restart --help
        Print this message.
";

/// Runs one `lane-restart` command line. `args` is the whole command line, program name first, as
/// `std::env::args_os()` gives it. (The old `main` took `std::env::args()`, which panics on a
/// non-UTF-8 argument; this reads it lossily instead.)
pub fn run(args: Vec<std::ffi::OsString>) -> ExitCode {
    run_with(&mut Ctx::real(), args)
}

/// As [`run`], with what a command may read from the outside world (and where its text goes) in
/// `ctx`. In-crate only: the public entry point is `cli::run`, which passes the REAL context. The
/// commands already moved to their own modules write through `ctx`; the rest still use the real
/// stdout and stderr and read the real environment until their own PRs (thin-main, one command per
/// PR).
pub(crate) fn run_with(ctx: &mut Ctx, args: Vec<std::ffi::OsString>) -> ExitCode {
    let argv: Vec<String> = args
        .into_iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        print_version();
        return ExitCode::SUCCESS;
    }
    if let [cmd, event] = argv.as_slice() {
        if cmd == "state" {
            return run_state_hook(event);
        }
    }
    if let Some((first, rest)) = argv.split_first() {
        if first == "assert-idle" {
            return match cli::assert_idle(rest) {
                Ok(role_override) => assert_idle_cmd::run(ctx, role_override),
                Err(e) => {
                    ctx.errln(&format!("lane-restart assert-idle: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "waits-poll" {
            return match cli::waits_poll(rest) {
                Ok(dry_run) => waits_poll_cmd::run(ctx, dry_run),
                Err(e) => {
                    ctx.errln(&format!("lane-restart waits-poll: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "waits" {
            return match cli::waits(rest) {
                Ok(role_override) => waits_cmd::run(ctx, role_override),
                Err(e) => {
                    ctx.errln(&format!("lane-restart waits: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "status" {
            return match cli::status(rest) {
                Ok((json, quiet_over, peers)) => status_cmd::run(ctx, json, quiet_over, peers),
                Err(e) => {
                    ctx.errln(&format!(
                        "lane-restart status: {e}; expected `status [--json] [--quiet-over <secs>] [--peers]`"
                    ));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "start" {
            return match cli::start(rest) {
                Ok(a) => start_cmd::run(ctx, a),
                Err(e) => {
                    ctx.errln(&format!("lane-restart start: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "stop" {
            return match cli::stop(rest) {
                Ok(a) => stop_cmd::run(ctx, a),
                Err(e) => {
                    ctx.errln(&format!("lane-restart stop: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "specs" {
            return match rest.split_first() {
                Some((sub, tail)) if sub == "import" => match cli::specs_import(tail) {
                    Ok((role, dry_run, verbose)) => specs_cmd::import(ctx, role, dry_run, verbose),
                    Err(e) => {
                        ctx.errln(&format!("lane-restart specs import: {e}"));
                        ExitCode::FAILURE
                    }
                },
                Some((sub, tail)) if sub == "retire" => match cli::specs_retire(tail) {
                    Ok((roles, dry_run)) => specs_cmd::retire(ctx, roles, dry_run),
                    Err(e) => {
                        ctx.errln(&format!("lane-restart specs retire: {e}"));
                        ExitCode::FAILURE
                    }
                },
                Some((sub, tail)) if sub == "synapse-only" => match cli::specs_synapse_only(tail) {
                    Ok((roles, strict, dry_run)) => {
                        specs_cmd::synapse_only(ctx, roles, strict, dry_run)
                    }
                    Err(e) => {
                        ctx.errln(&format!("lane-restart specs synapse-only: {e}"));
                        ExitCode::FAILURE
                    }
                },
                Some((sub, tail)) if sub == "both-channels" => {
                    match cli::specs_both_channels(tail) {
                        Ok((roles, dry_run)) => specs_cmd::both_channels(ctx, roles, dry_run),
                        Err(e) => {
                            ctx.errln(&format!("lane-restart specs both-channels: {e}"));
                            ExitCode::FAILURE
                        }
                    }
                }
                _ => {
                    ctx.errln(
                        "lane-restart specs: expected `specs import [--role <name>] [--dry-run] [--verbose]`, `specs retire --role <name>... [--dry-run]`, `specs synapse-only --role <name>... [--strict] [--dry-run]` or `specs both-channels --role <name>... [--dry-run]`",
                    );
                    ExitCode::FAILURE
                }
            };
        }
        if first == "lane-new" {
            return match cli::lane_new(rest) {
                Ok((role, repo, model, apply, plan)) => {
                    lane_new_cmd::run(ctx, role, repo, model, apply, plan)
                }
                Err(e) => {
                    ctx.errln(&format!(
                        "lane-restart lane-new: {e}; expected `lane-new <role> <repo> [--model sonnet|haiku] [--apply --plan <hash>]`"
                    ));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "install" {
            return match cli::install(rest) {
                Ok((from, dry_run)) => install_cmd::run(ctx, from, dry_run),
                Err(e) => {
                    ctx.errln(&format!(
                        "lane-restart install: {e}; expected `install --from <exe> [--dry-run]`"
                    ));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "ledger" {
            return match cli::ledger(rest) {
                Ok((f, json, since, by_lane)) => ledger_cmd::run(ctx, f, json, since, by_lane),
                Err(e) => {
                    ctx.errln(&format!("lane-restart ledger: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "models" && rest.is_empty() {
            print!("{}", relaunch::models_report(&state_dir()));
            return ExitCode::SUCCESS;
        }
        if first == "approvals" {
            return match cli::assert_idle(rest) {
                Ok(role) => approvals_cmd::run(ctx, role),
                Err(e) => {
                    ctx.errln(&format!("lane-restart approvals: {e}"));
                    ExitCode::FAILURE
                }
            };
        }
        if first == "attach" {
            return attach_cmd::run(ctx, rest);
        }
        if first == "host" {
            // Before anything can fail or wait: a restart that finds no record knows the host never ran.
            lane_restart::host_boot::record(&state_dir(), &argv[1..]);
            return match cli::host(rest) {
                Ok((role, child_argv)) => run_host_cmd(&role, &child_argv),
                Err(e) => {
                    eprintln!("lane-restart host: {e}");
                    ExitCode::FAILURE
                }
            };
        }
    }
    let args = match cli::restart(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lane-restart: {e}");
            return ExitCode::FAILURE;
        }
    };
    // A bare `--self` (the documented form): the role is the caller's own, found the way
    // `assert-idle` finds it. An explicit `--role` is never second-guessed here.
    let role = match args.role.clone() {
        Some(r) => r,
        None => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let claude_pid =
                lane_state_writer::claude_parent_pid(std::process::id(), &RealParentProcess::new())
                    .ok();
            match lane_restart::spec::resolve_self_role(
                &state_dir(),
                std::env::var("LANE_ROLE").ok().as_deref(),
                &cwd,
                claude_pid,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("lane-restart: --self without --role: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let target = if args.target_self {
        Target::Myself {
            role: role.clone(),
            // The same ancestry walk the hooks use to record `pid`: this
            // process <- shell(s) <- claude. `decide` compares it with the
            // state file's pid, so `--self` is a claim it can check.
            caller_pid: lane_state_writer::claude_parent_pid(
                std::process::id(),
                &RealParentProcess::new(),
            )
            .ok(),
        }
    } else {
        Target::Other { role: role.clone() }
    };

    let facts = SysinfoFacts::new(claude_config_dir());
    // 14.7 / 14.8: `requested_by` is the VERIFIED caller (the role whose claude session ran
    // this), not a label chosen by the absence of `--self`. A caller that cannot be verified is
    // recorded as "unverified"; a restart of another lane is still not refused on it (the
    // `--yes` + idle checks are unchanged), so the ledger is where a non-PM caller shows up.
    let caller = (!args.target_self).then(|| verified_caller(&facts));
    let caller_role = caller.as_ref().and_then(|c| c.as_ref().ok().cloned());
    let requested_by: &str = if args.target_self {
        "self"
    } else {
        caller_role.as_deref().unwrap_or("unverified")
    };
    // RESTART-TOOL-DESIGN.md 14.7 / 14.8, the PM's ruling of 2026-10-11: restarting ANOTHER lane
    // is the PM session's alone, the same rule as `stop`. Refused (and logged) before anything
    // about the target is read, even for a dry run.
    if let Some(caller) = caller {
        if let Err(refusal) = lane_stop::require_pm_for_other(false, caller) {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    }
    let req = authorize::Request {
        target,
        confirmed: args.confirmed,
        dry_run: args.dry_run,
    };

    let decision = authorize::decide(&req, &facts, &state_dir());
    // The target's own session and its host are what this restart replaces; any OTHER host or
    // claude of the role is a second session the state file may not show (2026-10-14).
    if let Ok(plan) = &decision {
        let names = lane_restart::duplicate::role_names(&state_dir(), &role);
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        if let Err(refusal) =
            lane_restart::duplicate::check(&facts, &role, &names, Some(plan.state.pid))
        {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
            return ExitCode::FAILURE;
        }
    }
    match decision {
        Err(refusal) => {
            eprintln!("lane-restart: refused - {refusal}");
            log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
            ExitCode::FAILURE
        }
        Ok(plan) if !plan.will_act => {
            match relaunch::describe_dry_run(&plan.state) {
                Ok(argv) => {
                    let cmdline: Vec<String> = argv
                        .iter()
                        .map(|a| {
                            if a.contains(' ') {
                                format!("{a:?}")
                            } else {
                                a.clone()
                            }
                        })
                        .collect();
                    println!(
                        "lane-restart: DRY RUN - would kill pid {} and relaunch a FRESH session \
                         via wt.exe (conhost.exe fallback), hosted by \
                         `lane-restart host --role {} -- {}`, in {}",
                        plan.state.pid,
                        plan.state.role,
                        cmdline.join(" "),
                        plan.state.cwd
                    );
                }
                Err(e) => {
                    println!(
                        "lane-restart: DRY RUN - would kill pid {} but the relaunch command \
                         itself would be refused: {e}",
                        plan.state.pid
                    );
                }
            }
            match tab_close::capture(&facts, plan.state.pid, &plan.identity) {
                Ok(shell) => println!(
                    "lane-restart: DRY RUN - would then close the leftover tab by terminating \
                     {shell}, if it has no other children by then"
                ),
                Err(why) => {
                    println!("lane-restart: DRY RUN - would leave the tab's shell alone: {why}")
                }
            }
            log_outcome(requested_by, &role, "dry run - no action taken", false);
            ExitCode::SUCCESS
        }
        Ok(plan) => {
            // Before the kill: after it, a refusal would leave a lane dead and not relaunched.
            if let Err(refusal) = disk_gate("restart") {
                log_outcome(requested_by, &role, &format!("refused: {refusal}"), false);
                return ExitCode::FAILURE;
            }
            eprintln!(
                "lane-restart: killing pid {} and relaunching a fresh session...",
                plan.state.pid
            );
            let state_dir_path = state_dir();
            let state_reader = relaunch::RealStateReader {
                state_dir: &state_dir_path,
            };
            let role_for_notice = plan.state.role.clone();
            let mut on_awaiting = || {
                // PM finding, 2026-09-18 (fifth real-restart retest):
                // printed immediately, the moment this is first detected -
                // never only at the end of the (up to ~15 minute) wait -
                // so whatever reads this stream (the PM lane, today) can
                // notify CireSnave promptly that the dev-channels dialog
                // needs a human.
                print_status_json("awaiting_confirmation", &role_for_notice);
            };
            // Captured while claude is still alive, moments before `kill_and_relaunch` kills it:
            // after the kill, claude's parent link is gone with it.
            let leftover_shell = tab_close::capture(&facts, plan.state.pid, &plan.identity);
            let relaunch_result = relaunch::kill_and_relaunch(
                &facts,
                &state_reader,
                &plan.state,
                &plan.identity,
                &mut on_awaiting,
            );
            // The tab closes LAST: on `--self`, this process may share the tab's console and go
            // with it, so everything else is already logged by then.
            let relaunch_acted = relaunch_result.is_ok();
            let exit = match relaunch_result {
                Ok(outcome @ relaunch::RelaunchOutcome::Relaunched) => {
                    log_outcome(
                        requested_by,
                        &role,
                        &format!(
                            "killed pid {} and relaunched a fresh session (continuity via HANDOFF only)",
                            plan.state.pid
                        ),
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Ok(outcome @ relaunch::RelaunchOutcome::AwaitingConfirmation) => {
                    log_outcome(
                        requested_by,
                        &role,
                        "awaiting human confirmation at the dev-channels dialog",
                        true,
                    );
                    relaunch_exit_code(outcome)
                }
                Err(e) => {
                    let note = identity_note(&e, plan.state.pid);
                    eprintln!("lane-restart: {e}{note}");
                    log_outcome(
                        requested_by,
                        &role,
                        &format!("action failed: {e}{note}"),
                        false,
                    );
                    ExitCode::FAILURE
                }
            };
            close_leftover_tab(
                &facts,
                "restart",
                requested_by,
                &role,
                leftover_shell,
                relaunch_acted,
                plan.state.pid,
                &plan.identity,
            );
            exit
        }
    }
}

/// Issue #14 (`docs/PORT-LINUX-PLAN.md`, P3'): when a kill was refused because the pid's identity no
/// longer matched, append what `/proc` says about its start so the log can tell a stepped clock
/// from a recycled pid. Empty for every other error. Changes no decision.
fn identity_note(e: &relaunch::RelaunchError, pid: u32) -> String {
    match e {
        relaunch::RelaunchError::Kill(lane_restart::facts::KillError::IdentityChanged) => {
            format!(" ({})", lane_restart::platform_diag(pid))
        }
        _ => String::new(),
    }
}

/// RESTART-TOOL-DESIGN.md §5: closes the tab a hand-started lane's shell keeps open once its
/// claude is killed (`tab_close.rs`). Only after a relaunch that acted - after a failed one the
/// tab stays, so a human has a prompt to relaunch from. Every outcome is logged, and a closed
/// shell is logged BEFORE the kill, in case closing the tab takes this process with it.
#[allow(clippy::too_many_arguments)]
fn close_leftover_tab(
    facts: &SysinfoFacts,
    kind: &str,
    requested_by: &str,
    role: &str,
    leftover_shell: Result<tab_close::ShellCandidate, tab_close::LeftAlone>,
    relaunch_acted: bool,
    killed_pid: u32,
    killed_identity: &lane_restart::facts::ProcessIdentity,
) {
    let result = leftover_shell.and_then(|shell| {
        if !relaunch_acted {
            return Err(tab_close::LeftAlone::RelaunchFailed);
        }
        tab_close::close(facts, &shell, killed_pid, killed_identity, &mut |shell| {
            log_kind(
                kind,
                requested_by,
                role,
                &format!("closing the leftover tab: terminating shell {shell}"),
                true,
            );
        })
    });
    if let Err(why) = result {
        log_kind(
            kind,
            requested_by,
            role,
            &format!("leftover tab's shell left alone: {why}"),
            false,
        );
    }
}

/// PM finding, 2026-09-19 (real-restart-2): exit 0 is indistinguishable
/// from `Relaunched` for anything scripting against this tool - a
/// genuinely different, real outcome needs a genuinely different exit
/// code. `AwaitingConfirmation` is real, ACTED work (§5) but not the same
/// as confirmed progress, so it gets its own code rather than sharing
/// `Relaunched`'s 0 or `Err`'s 1.
fn relaunch_exit_code(outcome: relaunch::RelaunchOutcome) -> ExitCode {
    match outcome {
        relaunch::RelaunchOutcome::Relaunched => ExitCode::SUCCESS,
        relaunch::RelaunchOutcome::AwaitingConfirmation => ExitCode::from(2),
    }
}

/// A machine-readable status line - PM finding, 2026-09-18 (fifth real-
/// restart retest): so the PM lane (or anything else watching this
/// process's stdout) can act on `awaiting_confirmation` promptly, e.g. by
/// sending CireSnave a phone notification that a relaunched lane needs a
/// human to confirm the dev-channels dialog.
fn print_status_json(outcome: &str, role: &str) {
    let line = serde_json::json!({
        "event": "lane-restart-status",
        "role": role,
        "outcome": outcome,
        "at": chrono::Utc::now().to_rfc3339(),
    });
    println!("{line}");
}

/// A restart-path ledger line (`kind: restart`).
fn log_outcome(requested_by: &str, role: &str, outcome: &str, acted: bool) {
    log_kind("restart", requested_by, role, outcome, acted);
}

/// A ledger line of `kind`, with `caller_role` set whenever `requested_by` is a verified role
/// (that is, neither the `--self` label nor "unverified").
fn log_kind(kind: &str, requested_by: &str, role: &str, outcome: &str, acted: bool) {
    let caller_role =
        (requested_by != "self" && requested_by != "unverified").then_some(requested_by);
    log_ledger(
        requested_by,
        role,
        kind,
        caller_role,
        None,
        None,
        outcome,
        acted,
    );
}

/// One `restart.log` line with the 14.7 fields.
#[allow(clippy::too_many_arguments)]
fn log_ledger(
    requested_by: &str,
    role: &str,
    kind: &str,
    caller_role: Option<&str>,
    model: Option<&str>,
    reason: Option<&str>,
    outcome: &str,
    acted: bool,
) {
    let entry = log::Entry {
        timestamp: chrono::Utc::now(),
        requested_by,
        role,
        outcome: outcome.to_string(),
        acted,
        kind: Some(kind),
        caller_role,
        model,
        reason,
        signal_id: None,
        result: None,
        spawned: None,
    };
    if let Err(e) = log::append(&log_path(), &entry) {
        eprintln!("lane-restart: WARNING - could not write to the restart log: {e}");
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_bare_self_is_accepted_and_the_role_is_resolved_later() {
        // The documented form (`lane-restart --self`, CLAUDE.md and CIRESNAVE-EXPECTATIONS 2.3b).
        let a = cli::restart(&strs(&["--self"])).expect("bare --self must parse");
        assert!(a.target_self);
        // without --self the role is still required
        assert!(cli::restart(&strs(&["--yes"])).is_err());
    }

    #[test]
    fn status_args_take_json_and_an_optional_positive_quiet_over_in_any_order() {
        assert_eq!(cli::status(&strs(&[])), Ok((false, None, false)));
        assert_eq!(cli::status(&strs(&["--json"])), Ok((true, None, false)));
        assert_eq!(
            cli::status(&strs(&["--quiet-over", "120", "--json"])),
            Ok((true, Some(120), false))
        );
        assert_eq!(
            cli::status(&strs(&["--peers", "--json"])),
            Ok((true, None, true))
        );
        for bad in [
            &["--quiet-over"][..],
            &["--quiet-over", "0"],
            &["--quiet-over", "-5"],
            &["--quiet-over", "2m"],
            &["--quiet-over", "1", "--quiet-over", "2"],
            &["--wat"],
        ] {
            assert!(cli::status(&strs(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn assert_idle_with_no_args_has_no_role_override() {
        assert_eq!(cli::assert_idle(&[]), Ok(None));
    }

    #[test]
    fn assert_idle_role_flag_is_extracted() {
        assert_eq!(
            cli::assert_idle(&strs(&["--role", "pm"])),
            Ok(Some("pm".to_string()))
        );
    }

    #[test]
    fn assert_idle_role_flag_with_no_value_is_an_error() {
        assert!(cli::assert_idle(&strs(&["--role"])).is_err());
    }

    #[test]
    fn assert_idle_rejects_an_unknown_flag() {
        assert!(cli::assert_idle(&strs(&["--bogus"])).is_err());
    }

    /// ⚠️ PM finding, 2026-09-18: `--help` printed "unrecognised argument:
    /// --help" because `cli::restart` treats every unknown token as an error
    /// with no earlier check. `main()` now intercepts `--help`/`-h` before
    /// any subcommand dispatch - this asserts the usage text actually
    /// documents the command this fix itself adds, so the two can't drift
    /// apart silently.
    #[test]
    fn usage_text_documents_assert_idle() {
        assert!(USAGE.contains("assert-idle"));
        assert!(USAGE.contains("--help"));
    }

    #[test]
    fn usage_text_documents_host_and_version() {
        assert!(USAGE.contains(" host "));
        assert!(USAGE.contains("--version"));
        assert!(USAGE.contains("lane-restart approvals"));
    }

    // -- relaunch_exit_code -------------------------------------------- //
    // PM finding, 2026-09-19 (real-restart-2): exit 0 was indistinguishable
    // from a genuine `Relaunched` for anything scripting against this tool.

    #[test]
    fn relaunched_exits_zero() {
        assert_eq!(
            relaunch_exit_code(relaunch::RelaunchOutcome::Relaunched),
            ExitCode::SUCCESS
        );
    }

    #[test]
    fn awaiting_confirmation_exits_two_not_zero() {
        let code = relaunch_exit_code(relaunch::RelaunchOutcome::AwaitingConfirmation);
        assert_ne!(
            code,
            ExitCode::SUCCESS,
            "AwaitingConfirmation must be distinguishable from Relaunched"
        );
        assert_eq!(code, ExitCode::from(2));
    }

    // -- start / specs import ------------------------------------------ //

    #[test]
    fn start_args_carry_role_reason_model_and_dry_run() {
        assert_eq!(
            cli::start(&strs(&[
                "--role",
                "baracuda",
                "--reason",
                "resume after crash",
                "--model",
                "haiku",
                "--dry-run",
                "--json",
                "--signal-id",
                "baracuda:7"
            ])),
            Ok(StartArgs {
                role: "baracuda".into(),
                reason: "resume after crash".into(),
                model: Some("haiku".into()),
                dry_run: true,
                json: true,
                signal_id: Some("baracuda:7".into()),
            })
        );
    }

    /// The reason is validated by `start::validate_reason`, once, so a missing one reaches it
    /// as an empty string rather than failing here with a second message.
    #[test]
    fn start_without_a_reason_parses_to_an_empty_reason() {
        let a = cli::start(&strs(&["--role", "baracuda"])).unwrap();
        assert_eq!(a.reason, "");
        assert!(start::validate_reason(&a.reason).is_err());
    }

    #[test]
    fn start_requires_a_role_and_values_for_its_flags() {
        assert!(cli::start(&strs(&["--reason", "x"])).is_err());
        assert!(cli::start(&strs(&["--role", "a", "--reason"])).is_err());
        assert!(cli::start(&strs(&["--role", "a", "--model"])).is_err());
        assert!(cli::start(&strs(&["--role", "a", "--bogus"])).is_err());
    }

    #[test]
    fn stop_args_are_role_reason_self_dry_run_and_yes() {
        assert_eq!(
            cli::stop(&strs(&[
                "--role",
                "baracuda",
                "--reason",
                "end of shift",
                "--self",
                "--dry-run",
                "--yes",
            ])),
            Ok(StopArgs {
                role: "baracuda".into(),
                reason: "end of shift".into(),
                target_self: true,
                dry_run: true,
                confirmed: true,
                kind: None,
            })
        );
        assert_eq!(
            cli::stop(&strs(&["--role", "a", "--soft"])).unwrap().kind,
            Some(StopKind::Soft)
        );
        assert_eq!(
            cli::stop(&strs(&["--role", "a", "--hard"])).unwrap().kind,
            Some(StopKind::Hard)
        );
        assert!(cli::stop(&strs(&["--role", "a", "--soft", "--hard"])).is_err());
        let a = cli::stop(&strs(&["--role", "baracuda"])).unwrap();
        assert_eq!(a.reason, "", "a missing reason reaches validate_reason");
        assert!(!a.target_self && !a.dry_run && !a.confirmed);
        assert!(cli::stop(&strs(&["--reason", "x"])).is_err());
        assert!(cli::stop(&strs(&["--role", "a", "--reason"])).is_err());
        assert!(cli::stop(&strs(&["--role", "a", "--model", "sonnet"])).is_err());
    }

    #[test]
    fn specs_import_args_are_role_and_dry_run() {
        assert_eq!(cli::specs_import(&[]), Ok((None, false, false)));
        assert_eq!(
            cli::specs_import(&strs(&["--role", "baracuda", "--dry-run", "--verbose"])),
            Ok((Some("baracuda".into()), true, true))
        );
        assert!(cli::specs_import(&strs(&["--yes"])).is_err());
    }

    #[test]
    fn specs_retire_args_need_a_role_and_repeat() {
        assert!(cli::specs_retire(&[]).is_err());
        assert_eq!(
            cli::specs_retire(&strs(&["--role", "a", "--role", "b", "--dry-run"])),
            Ok((vec!["a".to_string(), "b".to_string()], true))
        );
        assert!(cli::specs_retire(&strs(&["--all"])).is_err());
        assert!(USAGE.contains("lane-restart specs retire"));
    }

    #[test]
    fn install_args_need_from_and_refuse_the_unknown() {
        assert!(cli::install(&[]).is_err());
        assert_eq!(
            cli::install(&strs(&["--from", "x.exe", "--dry-run"])),
            Ok((PathBuf::from("x.exe"), true))
        );
        assert_eq!(
            cli::install(&strs(&["--from", "x.exe"])),
            Ok((PathBuf::from("x.exe"), false))
        );
        assert!(cli::install(&strs(&["--from"])).is_err());
        assert!(cli::install(&strs(&["--from", "x.exe", "--yes"])).is_err());
        assert!(USAGE.contains("lane-restart install --from"));
    }

    #[test]
    fn ledger_args_parse_filters_and_refuse_the_unknown() {
        let (f, json, since, by_lane) = cli::ledger(&strs(&[
            "--role",
            "a",
            "--kind",
            "start",
            "--refused",
            "--last",
            "5",
            "--since",
            "2h",
            "--json",
        ]))
        .unwrap();
        assert_eq!(f.role.as_deref(), Some("a"));
        assert_eq!(f.kind.as_deref(), Some("start"));
        assert_eq!((f.acted, f.last, json), (Some(false), Some(5), true));
        assert!(!by_lane);
        let (_, _, _, by_lane) = cli::ledger(&strs(&["--by-lane"])).unwrap();
        assert!(by_lane);
        assert_eq!(since.as_deref(), Some("2h"));
        assert!(cli::ledger(&strs(&["--last", "x"])).is_err());
        assert!(cli::ledger(&strs(&["--role"])).is_err());
        assert!(cli::ledger(&strs(&["--all"])).is_err());
        assert!(USAGE.contains("lane-restart ledger"));
    }

    #[test]
    fn usage_text_documents_start_and_specs_import() {
        assert!(USAGE.contains("lane-restart stop --role"));
        assert!(USAGE.contains("lane-restart start --role"));
        assert!(USAGE.contains("lane-restart specs import"));
    }

    // -- cli::host ---------------------------------------------------- //

    #[test]
    fn host_args_extracts_role_and_child_argv() {
        assert_eq!(
            cli::host(&strs(&[
                "--role", "overmind", "--", "claude", "--name", "x"
            ])),
            Ok(("overmind".to_string(), strs(&["claude", "--name", "x"])))
        );
    }

    #[test]
    fn host_args_requires_a_separator() {
        assert!(cli::host(&strs(&["--role", "overmind"])).is_err());
    }

    #[test]
    fn host_args_requires_a_non_empty_child_argv_after_the_separator() {
        assert!(cli::host(&strs(&["--role", "overmind", "--"])).is_err());
    }

    #[test]
    fn host_args_requires_role() {
        assert!(cli::host(&strs(&["--", "claude"])).is_err());
    }

    #[test]
    fn host_args_rejects_an_unknown_flag_before_the_separator() {
        assert!(cli::host(&strs(&["--bogus"])).is_err());
    }
}
