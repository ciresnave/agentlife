// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a `lane-restart` command may read from the outside world, and where its text goes.
//!
//! The seam (PM ruling A, 2026-11-01): `app::run_with(ctx, args)` takes a [`Ctx`], and
//! `cli::run` builds the REAL one with [`Ctx::real`]. Nothing here is public API: the public surface
//! stays `lane_restart::cli::run`, and the state directory stays the tool's fixed machine location
//! (an environment override of it was refused: a lane must not be able to point the supervisor tool
//! elsewhere). Only an in-crate `#[cfg(test)]` module can build a [`Ctx::capture`] with a state
//! directory of its own.

use crate::facts::{SysinfoFacts, SystemFacts};
use crate::waits_poll::{CheckRunner, ProcessRunner};
use std::path::PathBuf;

/// A notice to a lane: `(role, text)`.
type Sender = Box<dyn FnMut(&str, &str) -> Result<(), String>>;

/// Where one stream of text goes. `Real` is the process's own stdout or stderr, written with
/// `print!`/`eprint!` exactly as the commands wrote before this seam existed.
enum Sink {
    Stdout,
    Stderr,
    #[cfg(test)]
    Capture(String),
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
    // What a command reads from the machine that a test replaces. All three are `None` in the real
    // context and are BUILT ON DEMAND the way the commands built them before this seam: building
    // the real process facts resolves the Claude config directory, which panics when there is no
    // home, so a command that never asks for them (`--help`, `waits`) must never pay for them.
    facts: Option<Box<dyn SystemFacts>>,
    runner: Option<Box<dyn CheckRunner>>,
    sender: Option<Sender>,
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
            runner: None,
            sender: None,
        }
    }

    /// The process facts: the real `sysinfo` ones unless a test supplied its own.
    pub(crate) fn facts(&mut self) -> &dyn SystemFacts {
        if self.facts.is_none() {
            self.facts = Some(Box::new(SysinfoFacts::new(super::claude_config_dir())));
        }
        self.facts.as_deref().expect("set just above")
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
        let transport = crate::approvals::configured_transport(crate::paths::home_dir().as_deref());
        match transport {
            crate::notify::Transport::Broker => crate::notify::notify_role(
                crate::notify::broker_addr(),
                &self.state_dir,
                role,
                text,
            )
            .map(|_| ()),
            crate::notify::Transport::Synapse => {
                crate::notify::enqueue(&self.state_dir, role, text).map(|_| ())
            }
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
            runner: None,
            sender: None,
        }
    }

    /// Test only: these process facts instead of the real ones.
    #[cfg(test)]
    pub(crate) fn with_facts(mut self, facts: impl SystemFacts + 'static) -> Self {
        self.facts = Some(Box::new(facts));
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
}
