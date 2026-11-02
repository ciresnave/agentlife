// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a `lane-restart` command may read from the outside world, and where its text goes.
//!
//! The seam (PM ruling A, 2026-11-01): `app::run_with(ctx, args)` takes a [`Ctx`], and
//! `cli::run` builds the REAL one with [`Ctx::real`]. Nothing here is public API: the public surface
//! stays `lane_restart::cli::run`, and the state directory stays the tool's fixed machine location
//! (an environment override of it was refused: a lane must not be able to point the supervisor tool
//! elsewhere). Only an in-crate `#[cfg(test)]` module can build a [`Ctx::capture`] with a state
//! directory of its own.

use crate::approvals::{default_reader, RepoReader};
use crate::disk::{GuardOk, Refusal};
use crate::facts::{SysinfoFacts, SystemFacts};
use crate::host::HostOutcome;
use crate::install::Probe;
use crate::lane_new::Roots;
use crate::lane_new_host::Host;
use crate::lane_new_remote::Vcs;
use crate::lane_state_writer::{ParentProcess, RealParentProcess};
use crate::launch::{LivenessTiming, SpawnFn};
use crate::notify::{Peer, Transport};
use crate::start::CallerError;
use crate::waits_poll::{CheckRunner, ProcessRunner};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Runs the lane host: `(role, child argv)`.
type HostRunner = dyn Fn(&str, &[String]) -> std::io::Result<HostOutcome>;

/// A notice to a lane: `(role, text)`.
type Sender = Box<dyn FnMut(&str, &str) -> Result<(), String>>;

/// The claude-peers broker's live sessions.
type BrokerPeers = Box<dyn Fn() -> Result<Vec<Peer>, String>>;

/// The free-disk check for the volume holding a path (`disk::guard`).
type DiskGuard = Box<dyn Fn(&std::path::Path) -> Result<GuardOk, Refusal>>;

/// Who is running this command (`Ctx::verified_caller`).
type Caller = Box<dyn Fn() -> Result<String, CallerError>>;

/// Where one stream of text goes. `Real` is the process's own stdout or stderr, written with
/// `print!`/`eprint!` exactly as the commands wrote before this seam existed.
enum Sink {
    Stdout,
    Stderr,
    #[cfg(test)]
    Capture(String),
}

/// What `lane-new` reads and drives outside this process, as a test replaces it.
struct LaneNewWorld {
    roots: Result<Roots, String>,
    vcs: Option<Rc<dyn Vcs>>,
    host: Option<Rc<dyn Host>>,
}

pub(crate) struct Ctx {
    /// The lane state directory. For the real `Ctx` this is `lane_restart::state_dir()`.
    pub(crate) state_dir: PathBuf,
    /// The working directory, or why it could not be read.
    pub(crate) cwd: Result<PathBuf, String>,
    /// `$LANE_ROLE`.
    pub(crate) lane_role: Option<String>,
    out: Sink,
    err: Sink,
    // What a command reads from the machine that a test replaces. All are `None` in the real
    // context and are BUILT ON DEMAND the way the commands built them before this seam: building
    // the real process facts resolves the Claude config directory, which panics when there is no
    // home, so a command that never asks for them (`--help`, `waits`) must never pay for them. The
    // shared ones are handed out as owned `Rc` handles, so a command can hold several at once.
    facts: Option<Rc<dyn SystemFacts>>,
    parents: Option<Rc<dyn ParentProcess>>,
    /// The same walk as `parents`, kept as the concrete type when the real one was built, so the
    /// `state` hook can report how many whole-process scans it cost (`slow_note`).
    real_parents: Option<Rc<RealParentProcess>>,
    /// What the command reads as its standard input; `None` is the process's real stdin.
    stdin: Option<Box<dyn Read>>,
    runner: Option<Box<dyn CheckRunner>>,
    sender: Option<Sender>,
    transport: Option<Transport>,
    broker_peers: Option<BrokerPeers>,
    /// `Some(None)` is "there is no home directory"; `None` is "ask the machine".
    home: Option<Option<PathBuf>>,
    repo_reader: Option<Rc<dyn RepoReader>>,
    hooks_dir: Option<PathBuf>,
    probe: Option<Rc<dyn Probe>>,
    caller: Option<Caller>,
    lane_new: Option<LaneNewWorld>,
    launch_spawn: Option<Rc<SpawnFn>>,
    host_runner: Option<Rc<HostRunner>>,
    sleeper: Option<Rc<dyn Fn(Duration)>>,
    liveness: Option<LivenessTiming>,
    /// `Some`: these are ALL the environment variables the command can see through `env_var`.
    env: Option<HashMap<String, String>>,
    /// `Some(None)` is "free memory could not be read".
    free_memory_gb: Option<Option<f64>>,
    /// `None`: ask the machine's disk (`disk::guard`).
    disk: Option<DiskGuard>,
}

impl Ctx {
    /// The real machine: the fixed state directory, this process's working directory and
    /// environment, and its stdout and stderr.
    pub(crate) fn real() -> Self {
        Self {
            state_dir: crate::state_dir(),
            cwd: std::env::current_dir().map_err(|e| e.to_string()),
            lane_role: std::env::var("LANE_ROLE").ok(),
            out: Sink::Stdout,
            err: Sink::Stderr,
            facts: None,
            parents: None,
            real_parents: None,
            stdin: None,
            runner: None,
            sender: None,
            transport: None,
            broker_peers: None,
            home: None,
            repo_reader: None,
            hooks_dir: None,
            probe: None,
            caller: None,
            lane_new: None,
            launch_spawn: None,
            host_runner: None,
            sleeper: None,
            liveness: None,
            env: None,
            free_memory_gb: None,
            disk: None,
        }
    }

    /// Where the installed `lane-restart.exe` lives (the machine's hooks directory).
    pub(crate) fn hooks_dir(&self) -> PathBuf {
        self.hooks_dir
            .clone()
            .unwrap_or_else(crate::install::hooks_dir)
    }

    /// Runs a candidate binary for `install`: really, unless a test supplied its own.
    pub(crate) fn probe(&self) -> Rc<dyn Probe> {
        match &self.probe {
            Some(p) => Rc::clone(p),
            None => Rc::new(super::install_cmd::RealProbe),
        }
    }

    /// The roots `lane-new` derives every path from: the machine's layout under this context's state
    /// directory, unless a test supplied its own.
    pub(crate) fn lane_new_roots(&self) -> Result<Roots, String> {
        match &self.lane_new {
            Some(w) => w.roots.clone(),
            None => Roots::standard(self.state_dir.clone()),
        }
    }

    /// `gh` and `git` for `lane-new` (`None`: the local-file steps only, which only a test asks for).
    pub(crate) fn lane_new_vcs(&self) -> Option<Rc<dyn Vcs>> {
        match &self.lane_new {
            Some(w) => w.vcs.clone(),
            None => Some(Rc::new(crate::lane_new_remote::GhGit)),
        }
    }

    /// What `lane-new` runs outside this process (disk probe, provisioning script, `start`). Built
    /// only once the roots are known good, as before: the real one resolves the Claude config
    /// directory, which panics when there is no home.
    pub(crate) fn lane_new_host(&self, roots: &Roots) -> Option<Rc<dyn Host>> {
        match &self.lane_new {
            Some(w) => w.host.clone(),
            None => Some(Rc::new(crate::lane_new_host::RealHost::new(
                &roots.projects,
                &roots.lane_homes,
                &roots.pilot_home,
                super::claude_config_dir(),
            ))),
        }
    }

    /// The role whose `claude` session is running this command, verified by pid and start time
    /// (RESTART-TOOL-DESIGN.md 14.8), never by `LANE_ROLE`.
    pub(crate) fn verified_caller(&mut self) -> Result<String, CallerError> {
        if let Some(caller) = &self.caller {
            return caller();
        }
        let pid =
            crate::lane_state_writer::claude_parent_pid(std::process::id(), &*self.parents()).ok();
        let facts = self.facts();
        crate::start::caller_role(&self.state_dir, pid, &*facts)
    }

    /// The pid of the `claude` session running this command (the nearest `claude` ancestor), or
    /// `None` when there is none. `stop` needs it as well as the verified role.
    pub(crate) fn caller_claude_pid(&mut self) -> Option<u32> {
        crate::lane_state_writer::claude_parent_pid(std::process::id(), &*self.parents()).ok()
    }

    /// The caller of a `start`: the PM session by process (as for every command), else the launcher
    /// service by its record and by being this process's direct parent. Only `start` asks for the
    /// second; `stop` and a restart read `verified_caller` alone and so never see `overmind`.
    pub(crate) fn verified_start_caller(&mut self) -> Result<String, CallerError> {
        match self.verified_caller() {
            Ok(role) => Ok(role),
            Err(first) => {
                let parent = self
                    .parents()
                    .parent_of(std::process::id())
                    .map(|(pid, _)| pid);
                let facts = self.facts();
                crate::start::launcher_caller(&self.state_dir, parent, &*facts).map_err(|_| first)
            }
        }
    }

    /// Opens a lane's launch window and shell: really (`relaunch::spawn_launch`), unless a test
    /// supplied its own.
    pub(crate) fn launch_spawn(&self) -> Rc<SpawnFn> {
        match &self.launch_spawn {
            Some(f) => Rc::clone(f),
            None => Rc::new(crate::relaunch::spawn_launch),
        }
    }

    /// Runs the lane host (a ConPTY around `claude`): really, unless a test supplied its own.
    pub(crate) fn host_runner(&self) -> Rc<HostRunner> {
        match &self.host_runner {
            Some(f) => Rc::clone(f),
            None => Rc::new(crate::host::run),
        }
    }

    /// Waits `d`: really, unless a test supplied its own.
    pub(crate) fn sleeper(&self) -> Rc<dyn Fn(Duration)> {
        match &self.sleeper {
            Some(f) => Rc::clone(f),
            None => Rc::new(std::thread::sleep),
        }
    }

    /// How long a launch is given: the production timings, unless a test supplied its own.
    pub(crate) fn liveness_timing(&self) -> LivenessTiming {
        self.liveness.unwrap_or_default()
    }

    /// One environment variable (`None`: unset or not unicode), as `std::env::var(..).ok()`.
    pub(crate) fn env_var(&self, name: &str) -> Option<String> {
        match &self.env {
            Some(vars) => vars.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }

    /// The Opus permit: only when `LANE_RESTART_ALLOW_OPUS` is exactly `1` in the environment this
    /// context sees.
    pub(crate) fn opus_permit(&self) -> Option<crate::relaunch::OpusPermit> {
        crate::relaunch::OpusPermit::from_value(
            self.env_var(crate::relaunch::ALLOW_OPUS_ENV).as_deref(),
        )
    }

    /// The free-disk check for the volume holding this context's state directory: the machine's
    /// (`disk::guard`, a 20 GB floor) unless a test supplied its own. A test context (`capture`)
    /// defaults to a roomy disk, so no fixture test depends on how full the runner is (main's
    /// ubuntu run at 3d97be7 refused nine of them: `/` had 14.0 GB free).
    pub(crate) fn disk_guard(&self) -> Result<GuardOk, Refusal> {
        match &self.disk {
            Some(f) => f(&self.state_dir),
            None => crate::disk::guard(&self.state_dir),
        }
    }

    /// Free memory in GB, or `None` when it cannot be read.
    pub(crate) fn free_memory_gb(&self) -> Option<f64> {
        match self.free_memory_gb {
            Some(v) => v,
            None => crate::launcher_gate::free_memory_gb(),
        }
    }

    /// A `start` ledger line: `log_ledger`'s fields plus the signal it answered, its vocabulary-A
    /// result and whether a spawn was attempted (what the launcher's caps count).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_ledger_start(
        &mut self,
        requested_by: &str,
        role: &str,
        caller_role: Option<&str>,
        model: Option<&str>,
        reason: Option<&str>,
        outcome: &str,
        acted: bool,
        signal_id: Option<&str>,
        result: &str,
        spawned: bool,
    ) {
        let entry = crate::log::Entry {
            timestamp: chrono::Utc::now(),
            requested_by,
            role,
            outcome: outcome.to_string(),
            acted,
            kind: Some("start"),
            caller_role,
            model,
            reason,
            signal_id,
            result: Some(result),
            spawned: Some(spawned),
        };
        if let Err(e) = crate::log::append(&self.state_dir.join("restart.log"), &entry) {
            self.errln(&format!(
                "lane-restart: WARNING - could not write to the restart log: {e}"
            ));
        }
    }

    /// One `restart.log` line with the 14.7 fields, in this context's state directory. A write that
    /// fails is a warning on stderr, never a failure of the command.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_ledger(
        &mut self,
        requested_by: &str,
        role: &str,
        kind: &str,
        caller_role: Option<&str>,
        model: Option<&str>,
        reason: Option<&str>,
        outcome: &str,
        acted: bool,
    ) {
        let entry = crate::log::Entry {
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
        if let Err(e) = crate::log::append(&self.state_dir.join("restart.log"), &entry) {
            self.errln(&format!(
                "lane-restart: WARNING - could not write to the restart log: {e}"
            ));
        }
    }

    /// The user's home directory (where `~/.overmind/lane-restart.json` lives), or `None` when the
    /// machine has none.
    pub(crate) fn home(&self) -> Option<PathBuf> {
        match &self.home {
            Some(h) => h.clone(),
            None => super::dirs_home(),
        }
    }

    /// The reader the approvals are fetched with: `gh` under a time budget, cached by commit under
    /// `home`, unless a test supplied its own.
    pub(crate) fn repo_reader(&self, home: &std::path::Path) -> Rc<dyn RepoReader> {
        match &self.repo_reader {
            Some(r) => Rc::clone(r),
            None => Rc::new(default_reader(home)),
        }
    }

    /// The process facts: the real `sysinfo` ones unless a test supplied its own.
    pub(crate) fn facts(&mut self) -> Rc<dyn SystemFacts> {
        if self.facts.is_none() {
            self.facts = Some(Rc::new(SysinfoFacts::new(super::claude_config_dir())));
        }
        Rc::clone(self.facts.as_ref().expect("set just above"))
    }

    /// The process-parent walk: the real one unless a test supplied its own.
    pub(crate) fn parents(&mut self) -> Rc<dyn ParentProcess> {
        if self.parents.is_none() {
            let real = Rc::new(RealParentProcess::new());
            self.real_parents = Some(Rc::clone(&real));
            self.parents = Some(real);
        }
        Rc::clone(self.parents.as_ref().expect("set just above"))
    }

    /// How many times the REAL process-parent walk scanned the whole process table (0 when a test
    /// supplied its own walk, or none was built).
    pub(crate) fn parent_full_scans(&self) -> u32 {
        self.real_parents.as_ref().map_or(0, |p| p.full_scans())
    }

    /// The command's standard input: the process's own, unless a test supplied one. Handed out once.
    pub(crate) fn stdin(&mut self) -> Box<dyn Read> {
        self.stdin
            .take()
            .unwrap_or_else(|| Box::new(std::io::stdin()))
    }

    /// How notices travel (`notify_transport` in `~/.overmind/lane-restart.json`; the broker when
    /// there is no file): read when asked, as before.
    pub(crate) fn transport(&self) -> Transport {
        self.transport.unwrap_or_else(|| {
            crate::approvals::configured_transport(crate::paths::home_dir().as_deref())
        })
    }

    /// The claude-peers broker's live sessions, or why they could not be read.
    pub(crate) fn broker_peers(&self) -> Result<Vec<Peer>, String> {
        match &self.broker_peers {
            Some(f) => f(),
            None => crate::notify::list_all(crate::notify::broker_addr()),
        }
    }

    /// Runs a `waits-poll` check: the real direct-exec runner unless a test supplied its own.
    pub(crate) fn runner(&mut self) -> &dyn CheckRunner {
        if self.runner.is_none() {
            self.runner = Some(Box::new(ProcessRunner::default()));
        }
        self.runner.as_deref().expect("set just above")
    }

    /// Sends `text` to `role`'s lane on the configured `notify_transport` (the claude-peers broker,
    /// or the Synapse outbox), unless a test supplied its own sender.
    pub(crate) fn send(&mut self, role: &str, text: &str) -> Result<(), String> {
        if let Some(sender) = self.sender.as_mut() {
            return sender(role, text);
        }
        match self.transport() {
            Transport::Broker => crate::notify::notify_role(
                crate::notify::broker_addr(),
                &self.state_dir,
                role,
                text,
            )
            .map(|_| ()),
            Transport::Synapse => crate::notify::enqueue(&self.state_dir, role, text).map(|_| ()),
        }
    }

    /// Text to stdout, as `print!`.
    pub(crate) fn out(&mut self, text: &str) {
        Self::write(&mut self.out, text);
    }

    /// A line to stdout, as `println!`.
    pub(crate) fn outln(&mut self, text: &str) {
        Self::write(&mut self.out, &format!("{text}\n"));
    }

    /// A line to stderr, as `eprintln!`.
    pub(crate) fn errln(&mut self, text: &str) {
        Self::write(&mut self.err, &format!("{text}\n"));
    }

    fn write(sink: &mut Sink, text: &str) {
        match sink {
            Sink::Stdout => print!("{text}"),
            Sink::Stderr => eprint!("{text}"),
            #[cfg(test)]
            Sink::Capture(buf) => buf.push_str(text),
        }
    }

    /// A `Ctx` whose text is kept, for a test: the given state directory, working directory and
    /// `$LANE_ROLE`.
    #[cfg(test)]
    pub(crate) fn capture(
        state_dir: PathBuf,
        cwd: Result<PathBuf, String>,
        lane_role: Option<&str>,
    ) -> Self {
        Self {
            state_dir,
            cwd,
            lane_role: lane_role.map(str::to_string),
            out: Sink::Capture(String::new()),
            err: Sink::Capture(String::new()),
            facts: None,
            parents: None,
            real_parents: None,
            stdin: None,
            runner: None,
            sender: None,
            transport: None,
            broker_peers: None,
            home: None,
            repo_reader: None,
            hooks_dir: None,
            probe: None,
            caller: None,
            lane_new: None,
            launch_spawn: None,
            host_runner: None,
            sleeper: None,
            liveness: None,
            env: None,
            free_memory_gb: None,
            disk: Some(Box::new(|_| Ok(roomy_disk()))),
        }
    }

    /// Test only: this hooks directory instead of the machine's.
    #[cfg(test)]
    pub(crate) fn with_hooks_dir(mut self, dir: PathBuf) -> Self {
        self.hooks_dir = Some(dir);
        self
    }

    /// Test only: this probe instead of really running the candidate.
    #[cfg(test)]
    pub(crate) fn with_probe(mut self, probe: impl Probe + 'static) -> Self {
        self.probe = Some(Rc::new(probe));
        self
    }

    /// Test only: this verified caller instead of the process-ancestry lookup.
    #[cfg(test)]
    pub(crate) fn with_caller(
        mut self,
        caller: impl Fn() -> Result<String, CallerError> + 'static,
    ) -> Self {
        self.caller = Some(Box::new(caller));
        self
    }

    /// Test only: these roots, GitHub client and host for `lane-new` instead of the machine's.
    #[cfg(test)]
    pub(crate) fn with_lane_new(
        mut self,
        roots: Result<Roots, String>,
        vcs: Option<Rc<dyn Vcs>>,
        host: Option<Rc<dyn Host>>,
    ) -> Self {
        self.lane_new = Some(LaneNewWorld { roots, vcs, host });
        self
    }

    /// Test only: this launch spawner instead of really opening a window.
    #[cfg(test)]
    pub(crate) fn with_launch_spawn(mut self, spawn: Rc<SpawnFn>) -> Self {
        self.launch_spawn = Some(spawn);
        self
    }

    /// Test only: this host instead of really opening a ConPTY.
    #[cfg(test)]
    pub(crate) fn with_host_runner(mut self, runner: Rc<HostRunner>) -> Self {
        self.host_runner = Some(runner);
        self
    }

    /// Test only: this wait instead of really sleeping.
    #[cfg(test)]
    pub(crate) fn with_sleeper(mut self, sleeper: Rc<dyn Fn(Duration)>) -> Self {
        self.sleeper = Some(sleeper);
        self
    }

    /// Test only: these launch timings instead of the production ones.
    #[cfg(test)]
    pub(crate) fn with_liveness_timing(mut self, timing: LivenessTiming) -> Self {
        self.liveness = Some(timing);
        self
    }

    /// Test only: exactly these environment variables.
    #[cfg(test)]
    pub(crate) fn with_env(mut self, vars: &[(&str, &str)]) -> Self {
        self.env = Some(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        self
    }

    /// Test only: this free memory (`None`: unreadable) instead of the machine's.
    #[cfg(test)]
    pub(crate) fn with_free_memory_gb(mut self, gb: Option<f64>) -> Self {
        self.free_memory_gb = Some(gb);
        self
    }

    /// Test only: this home directory (`None`: the machine has none) instead of the real one.
    #[cfg(test)]
    pub(crate) fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = Some(home);
        self
    }

    /// Test only: this approvals reader instead of `gh`.
    #[cfg(test)]
    pub(crate) fn with_repo_reader(mut self, reader: impl RepoReader + 'static) -> Self {
        self.repo_reader = Some(Rc::new(reader));
        self
    }

    /// Test only: these process facts instead of the real ones.
    #[cfg(test)]
    pub(crate) fn with_facts(mut self, facts: impl SystemFacts + 'static) -> Self {
        self.facts = Some(Rc::new(facts));
        self
    }

    /// Test only: this text as the command's standard input.
    #[cfg(test)]
    pub(crate) fn with_stdin(mut self, text: &str) -> Self {
        self.stdin = Some(Box::new(std::io::Cursor::new(text.as_bytes().to_vec())));
        self
    }

    /// Test only: this process-parent walk instead of the real one.
    #[cfg(test)]
    pub(crate) fn with_parents(mut self, parents: impl ParentProcess + 'static) -> Self {
        self.parents = Some(Rc::new(parents));
        self
    }

    /// Test only: this disk check instead of the roomy default.
    #[cfg(test)]
    pub(crate) fn with_disk(
        mut self,
        guard: impl Fn(&std::path::Path) -> Result<GuardOk, Refusal> + 'static,
    ) -> Self {
        self.disk = Some(Box::new(guard));
        self
    }

    /// Test only: this notify transport instead of the configured one.
    #[cfg(test)]
    pub(crate) fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Test only: this broker answer instead of the live broker's.
    #[cfg(test)]
    pub(crate) fn with_broker_peers(
        mut self,
        peers: impl Fn() -> Result<Vec<Peer>, String> + 'static,
    ) -> Self {
        self.broker_peers = Some(Box::new(peers));
        self
    }

    /// Test only: this check runner instead of the real one.
    #[cfg(test)]
    pub(crate) fn with_runner(mut self, runner: impl CheckRunner + 'static) -> Self {
        self.runner = Some(Box::new(runner));
        self
    }

    /// Test only: this sender instead of the transport.
    #[cfg(test)]
    pub(crate) fn with_sender(
        mut self,
        sender: impl FnMut(&str, &str) -> Result<(), String> + 'static,
    ) -> Self {
        self.sender = Some(Box::new(sender));
        self
    }

    /// What a `capture` context has been given so far: `(stdout, stderr)`.
    #[cfg(test)]
    pub(crate) fn captured(&self) -> (String, String) {
        let text = |s: &Sink| match s {
            Sink::Capture(b) => b.clone(),
            _ => String::new(),
        };
        (text(&self.out), text(&self.err))
    }
}

/// A volume with room to spare, for a test context's default disk.
#[cfg(test)]
fn roomy_disk() -> GuardOk {
    GuardOk::Free(crate::disk::Free {
        mount: "/".to_string(),
        bytes: 1 << 50,
    })
}

/// Raw bytes to stdout, for output that is not text (the watched screen of `attach`): written to
/// the process's real stdout, never through a text print, which would mangle a terminal stream.
impl std::io::Write for Ctx {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.out {
            Sink::Stdout => std::io::stdout().write(buf),
            Sink::Stderr => std::io::stderr().write(buf),
            #[cfg(test)]
            Sink::Capture(text) => {
                text.push_str(&String::from_utf8_lossy(buf));
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.out {
            Sink::Stdout => std::io::stdout().flush(),
            Sink::Stderr => std::io::stderr().flush(),
            #[cfg(test)]
            Sink::Capture(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production location cannot drift silently: the real context's state directory is the
    /// tool's fixed machine path, whatever a test or an environment says.
    #[test]
    fn the_real_context_uses_the_fixed_machine_state_dir() {
        let ctx = Ctx::real();
        assert_eq!(ctx.state_dir, crate::state_dir());
        #[cfg(windows)]
        assert_eq!(ctx.state_dir, PathBuf::from("C:/Projects/.lane-state"));
        #[cfg(not(windows))]
        assert_eq!(
            ctx.state_dir.file_name().and_then(|n| n.to_str()),
            Some("lane-state")
        );
    }

    #[test]
    fn a_capture_context_keeps_each_stream_apart() {
        let mut ctx = Ctx::capture(PathBuf::from("s"), Ok(PathBuf::from("c")), Some("r"));
        ctx.out("a");
        ctx.outln("b");
        ctx.errln("c");
        assert_eq!(ctx.captured(), ("ab\n".to_string(), "c\n".to_string()));
        assert_eq!(ctx.lane_role.as_deref(), Some("r"));
    }

    /// The Opus permit is exactly `LANE_RESTART_ALLOW_OPUS=1` in the environment the context sees,
    /// and nothing else (CireSnave cannot afford Opus: unset, `0`, `true` and an empty value all
    /// mean no permit).
    #[test]
    fn the_opus_permit_is_only_the_exact_value_1_in_the_contexts_environment() {
        let permit = |vars: &[(&str, &str)]| {
            Ctx::capture(PathBuf::from("s"), Ok(PathBuf::from("c")), None)
                .with_env(vars)
                .opus_permit()
                .is_some()
        };
        assert!(permit(&[("LANE_RESTART_ALLOW_OPUS", "1")]), "control");
        assert!(!permit(&[]));
        for no in ["0", "true", "", "yes", "11", " 1"] {
            assert!(!permit(&[("LANE_RESTART_ALLOW_OPUS", no)]), "{no:?}");
        }
        assert!(!permit(&[("SOMETHING_ELSE", "1")]));
    }
}
