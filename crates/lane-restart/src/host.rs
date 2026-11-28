// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart host --role <role> -- <argv…>` — RESTART-TOOL-DESIGN.md §12.5.
//!
//! Runs INSIDE the relaunched session's own terminal tab (`wt.exe`, per §5)
//! and hosts the real child (`claude`) inside a ConPTY THIS process creates
//! and owns — never attaching to anyone else's console. `AttachConsole`
//! against an externally-owned, ConPTY-hosted console was proven NOT to
//! work reliably (empirically, live, 2026-09-18); owning both ends of a
//! ConPTY this process creates itself sidesteps that entirely, and was
//! proven working the same day (real `CreatePseudoConsole` spawn, real
//! read, real inject, marker confirmed round-trip).
//!
//! ⚠️ TRANSPARENT FROM BYTE 0 (PM finding, 2026-09-18): in production the
//! host's own stdin/stdout ARE the real terminal (`wt` itself answers
//! `claude`'s cursor-position-report queries and other terminal queries,
//! the same way any real terminal answers any real TUI). Every byte the
//! child writes goes to our own stdout untouched; every byte we read from
//! our own stdin goes to the child untouched. This module never answers a
//! CPR itself — that was only ever needed in the scratch probe because
//! nothing was on the other end of a throwaway pipe. The host only
//! OBSERVES the output stream (via a `vt100` screen model) to check for a
//! handler match during the STARTUP WINDOW; matching never withholds,
//! delays, or rewrites a single byte of the real relay - see
//! `relay_chunk`'s own test for the property this claims.

use crate::handlers::{self, HandlerSpec};
use crate::{approvals, notify};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long after the child starts the host still watches for a handler
/// match. After this, it's pure passthrough for the rest of the session's
/// life - matching a dialog long past when a real interactive session is
/// expected to be running isn't this mechanism's job (the OUTER
/// `lane-restart --role …` process's own liveness check, §5, is what
/// actually times a stuck session out).
pub const STARTUP_WINDOW: Duration = Duration::from_secs(60);

/// A screen that is only a dialog footer this long gets a repaint request, up to `MAX_REPAINTS`.
const REPAINT_AFTER: Duration = Duration::from_secs(2);
const MAX_REPAINTS: u32 = 5;

/// A polled console smaller than this is not a real terminal (PM ruling toupv5-jds-jw, 2026-11-26:
/// launcher starts collapsed the pty to 2 rows x 46 columns or about 10 columns wide). Logged, never forwarded.
const MIN_PLAUSIBLE_ROWS: u16 = 10;
const MIN_PLAUSIBLE_COLS: u16 = 40;
/// The size the pty is redrawn at, once per start, when the nudges did not bring the dialog body back.
const REDRAW_SIZE: (u16, u16) = (120, 30);
/// The repaint count after which the one REDRAW is asked for.
const REDRAW_AFTER_REPAINTS: u32 = 2;

/// What the resize poll does with the console size it just read: `Some(size)` to forward it to the pty,
/// `None` to leave the pty alone (unchanged, or degenerate).
fn forwardable_console_size(last: (u16, u16), polled: (u16, u16)) -> Option<(u16, u16)> {
    let (c, r) = polled;
    (polled != last && r >= MIN_PLAUSIBLE_ROWS && c >= MIN_PLAUSIBLE_COLS).then_some(polled)
}

/// What one resize poll does (L1/N4 of Pip's review of #304): forward a plausible new size, note a degenerate
/// one ONCE (not every 500 ms while the console stays that way), or nothing. `noted` is the degenerate size
/// already logged; it is cleared when the console is plausible again or back at the pty's own size.
#[derive(Debug, PartialEq, Eq)]
enum PollAction {
    Forward((u16, u16)),
    NoteDegenerate,
    Nothing,
}

fn poll_action(last: (u16, u16), polled: (u16, u16), noted: &mut Option<(u16, u16)>) -> PollAction {
    if let Some(size) = forwardable_console_size(last, polled) {
        *noted = None;
        return PollAction::Forward(size);
    }
    if polled == last {
        *noted = None;
        return PollAction::Nothing;
    }
    if *noted == Some(polled) {
        return PollAction::Nothing;
    }
    *noted = Some(polled);
    PollAction::NoteDegenerate
}

/// The size the pty is created at, and where it came from (L2 of Pip's review of #304): the host's own console
/// when it is plausible, else 80x25 with the reason, so a degenerate creation size never sticks for the session.
fn creation_size(console: Option<(u16, u16)>) -> ((u16, u16), String) {
    match console {
        Some((c, r)) if r >= MIN_PLAUSIBLE_ROWS && c >= MIN_PLAUSIBLE_COLS => (
            (c, r),
            "the host's own console (GetConsoleScreenBufferInfo / tcgetwinsize)".to_string(),
        ),
        Some((c, r)) => (
            (80, 25),
            format!("FALLBACK 80x25: the console reported a degenerate size {c}x{r} (cols x rows)"),
        ),
        None => (
            (80, 25),
            "FALLBACK 80x25: the console size query failed".to_string(),
        ),
    }
}

/// The host-log line when the one REDRAW is asked for. The resize itself happens on the resize thread, which logs
/// its own outcome as `PTY SIZE REDRAW`; this line says only that it was requested.
fn redraw_request_note(repaints: u32) -> String {
    format!(
        "REDRAW requested: still only a dialog footer after {repaints} repaints, asking for one resize to {}x{} (the resize thread logs `PTY SIZE REDRAW` with its outcome); a footer is never answered, only the handlers answer a matched dialog",
        REDRAW_SIZE.0, REDRAW_SIZE.1
    )
}

/// Where the lane's Synapse adapter looks for host requests: the `--home` and `--role` its `--mcp-config` file gives
/// the `synapse` server (the adapter's own `home` and `role`; the host's role is the fallback for a file with none).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SynapseTarget {
    home: std::path::PathBuf,
    role: String,
}

fn synapse_target(host_role: &str, child_argv: &[String]) -> Option<SynapseTarget> {
    let path = crate::spec_synapse::mcp_config_arg(child_argv)?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let home = crate::spec_audit::mcp_home(&v)?;
    let role = v
        .get("mcpServers")
        .and_then(|m| m.get("synapse"))
        .and_then(|s| s.get("args"))
        .and_then(serde_json::Value::as_array)
        .and_then(|a| {
            let i = a.iter().position(|x| x.as_str() == Some("--role"))?;
            a.get(i + 1)?.as_str().map(str::to_string)
        })
        .unwrap_or_else(|| host_role.to_string());
    Some(SynapseTarget {
        home: std::path::PathBuf::from(home),
        role,
    })
}

/// The file a host creates in the Synapse home to ask `role`'s adapter to write its unacked mail again. Mirrors
/// synapse's `redeliver_request_path` (branch lane/h1-redeliver d7f0bdf3): ASCII alphanumerics, `-` and `_` are
/// kept, anything else becomes `_`.
fn redeliver_request_path(home: &std::path::Path, role: &str) -> std::path::PathBuf {
    let safe: String = role
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    home.join(format!("redeliver-{safe}.request"))
}

/// Right after the host answers a dev-channels dialog, and before the lane's first prompt: ask the adapter to write
/// the lane's unacked mail again (H1, `docs/MAIL-BEFORE-FIRST-TURN.md`). `None` for any other dialog; otherwise the
/// host-log line. A failure is that line, never a failed start.
fn request_redeliver_after_answer(
    handler_id: &str,
    target: Option<&SynapseTarget>,
) -> Option<String> {
    if !handler_id.contains("dev-channel") {
        return None;
    }
    let Some(t) = target else {
        return Some(
            "REDELIVER not requested: no Synapse home in the lane's --mcp-config".to_string(),
        );
    };
    let path = redeliver_request_path(&t.home, &t.role);
    Some(
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => format!("REDELIVER requested: created {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                format!("REDELIVER request already pending: {}", path.display())
            }
            Err(e) => format!("REDELIVER not requested: {}: {e}", path.display()),
        },
    )
}

/// True when the screen holds exactly one non-blank line and it is a dialog footer
/// ("Enter to confirm", "Esc to cancel"): the footer of a dialog whose body never reached the
/// screen model, which no handler can match.
fn footer_only(screen: &str) -> bool {
    let mut lines = screen.lines().map(str::trim).filter(|l| !l.is_empty());
    match (lines.next(), lines.next()) {
        (Some(line), None) => {
            let lower = line.to_ascii_lowercase();
            lower.contains("enter to confirm") || lower.contains("esc to cancel")
        }
        _ => false,
    }
}

/// ⚠️ ROOT CAUSE, PM finding 2026-09-19 (real-restart-3, raw I/O log): the
/// CPR went out (`child->relay n=4 "\x1b[6n"`) and NOTHING ever came back
/// on stdin - zero `outer_stdin->child` entries. Windows consoles start in
/// COOKED mode: `ENABLE_LINE_INPUT` buffers input until Enter, and WT's
/// CPR reply (`ESC[r;cR`) has no Enter, so a `ReadFile` on the host's own
/// stdin never returns it; without `ENABLE_VIRTUAL_TERMINAL_INPUT` it may
/// not even be delivered as VT bytes. This has nothing to do with the
/// relay LOGIC (proven sound by this module's own output-driven-CPR test):
/// it's the host's own CONSOLE MODE, set once at production startup
/// (`run()`, never here - a test's fake pipes have no console mode to set)
/// and always restored, including on panic (`Drop`).
#[cfg(windows)]
struct RawConsoleGuard {
    stdin_handle: windows_sys::Win32::Foundation::HANDLE,
    original_stdin_mode: Option<u32>,
    stdout_handle: windows_sys::Win32::Foundation::HANDLE,
    original_stdout_mode: Option<u32>,
}

#[cfg(windows)]
impl RawConsoleGuard {
    /// Only touches a handle whose `GetConsoleMode` succeeds - i.e. only a
    /// REAL console, never a pipe or file (a test's fake stdin/stdout, or a
    /// real launch whose std handles were redirected, are both left alone).
    fn enable() -> Self {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, DISABLE_NEWLINE_AUTO_RETURN,
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
            ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE,
        };
        unsafe {
            let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut stdin_mode: u32 = 0;
            let original_stdin_mode = if GetConsoleMode(stdin_handle, &mut stdin_mode) != 0 {
                let new_mode = (stdin_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
                    & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
                SetConsoleMode(stdin_handle, new_mode);
                Some(stdin_mode)
            } else {
                None
            };

            let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut stdout_mode: u32 = 0;
            let original_stdout_mode = if GetConsoleMode(stdout_handle, &mut stdout_mode) != 0 {
                let new_mode =
                    stdout_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN;
                SetConsoleMode(stdout_handle, new_mode);
                Some(stdout_mode)
            } else {
                None
            };

            Self {
                stdin_handle,
                original_stdin_mode,
                stdout_handle,
                original_stdout_mode,
            }
        }
    }
}

#[cfg(windows)]
impl Drop for RawConsoleGuard {
    /// Restores BOTH original modes on every exit path - normal return, an
    /// early `?`, or a panic unwind - never leaves the real console (which
    /// outlives this process, since it's WT's own) in raw mode.
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::SetConsoleMode;
        unsafe {
            if let Some(mode) = self.original_stdin_mode {
                SetConsoleMode(self.stdin_handle, mode);
            }
            if let Some(mode) = self.original_stdout_mode {
                SetConsoleMode(self.stdout_handle, mode);
            }
        }
    }
}

/// The unix counterpart of the guard above (`docs/PORT-LINUX-PLAN.md`, slice P4). A terminal starts
/// in canonical mode: the line discipline holds input until Enter and echoes it, so the terminal's
/// own reply to `claude`'s cursor-position query (`ESC[r;cR`, no Enter) would never reach the child.
/// Same shape as the Windows guard: only a REAL tty is touched (a pipe, a file or a test's fake
/// stdin is left alone), and the original mode is restored on every exit path, panic unwind
/// included.
#[cfg(unix)]
struct RawConsoleGuard {
    saved: Option<(std::os::fd::OwnedFd, rustix::termios::Termios)>,
}

#[cfg(unix)]
impl RawConsoleGuard {
    fn enable() -> Self {
        use std::os::fd::AsFd;
        Self::enable_on(std::io::stdin().as_fd())
    }

    fn enable_on(fd: std::os::fd::BorrowedFd<'_>) -> Self {
        use rustix::termios::{isatty, tcgetattr, tcsetattr, OptionalActions};
        let none = Self { saved: None };
        if !isatty(fd) {
            return none;
        }
        let Ok(original) = tcgetattr(fd) else {
            return none;
        };
        let Ok(owned) = fd.try_clone_to_owned() else {
            return none;
        };
        let mut raw = original.clone();
        raw.make_raw();
        if tcsetattr(&owned, OptionalActions::Now, &raw).is_err() {
            return none;
        }
        Self {
            saved: Some((owned, original)),
        }
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.saved.is_some()
    }
}

#[cfg(unix)]
impl Drop for RawConsoleGuard {
    fn drop(&mut self) {
        use rustix::termios::{tcsetattr, OptionalActions};
        if let Some((fd, original)) = &self.saved {
            let _ = tcsetattr(fd, OptionalActions::Now, original);
        }
    }
}

/// A direct launch (`docs/PORT-LINUX-PLAN.md`, P6) starts the host with no terminal: its stdin is the
/// null device. Such a host must not share its launcher's session, or closing the launcher's
/// terminal (SIGHUP to the session) would end every lane it started. `setsid` makes the host the
/// leader of a new session. A host that HAS a tty (a person ran it in a terminal) is left alone: it
/// belongs to that terminal. Failure (already a session leader) is not an error.
#[cfg(unix)]
fn detach_session_if_headless() {
    use std::os::fd::AsFd;
    if !rustix::termios::isatty(std::io::stdin().as_fd()) {
        let _ = rustix::process::setsid();
    }
}

/// What an automatic answer gets logged with - `restart.log` (§12.4: "log
/// the automatic answer... with the handler's id and the exact text that
/// matched").
pub struct HandlerAnswered {
    pub handler_id: String,
    pub matched_screen_text: String,
    /// The literal bytes just written to the child's stdin - PM finding,
    /// 2026-09-19: "any injection with the bytes" needs the actual bytes on
    /// the record, not just that an injection happened.
    pub injected_action: String,
    /// For a `folder_prefix` (folder-trust) approval: the canonical folder it just trusted. Every such
    /// answer is written to `folder-trust-answers.log` and announced to the notify role.
    pub resolved_folder: Option<String>,
}

/// Relays one chunk of the CHILD's real output to `output_writer`
/// UNCHANGED, then (only after the write) feeds the same bytes into
/// `parser` for observation. The write always happens first and always
/// happens in full - matching never withholds, delays, or alters a single
/// byte of the real relay.
pub fn relay_chunk(
    chunk: &[u8],
    output_writer: &mut impl Write,
    parser: &mut vt100::Parser,
) -> std::io::Result<()> {
    output_writer.write_all(chunk)?;
    output_writer.flush()?;
    parser.process(chunk);
    Ok(())
}

/// Relays what the reader sends after the main loop has stopped, until the reader ends (its
/// sender is dropped) or `deadline` passes. Returns how many chunks it relayed. A write error
/// stops the relay (the outer side is gone) but never panics or hangs the teardown.
#[must_use]
pub fn drain_tail(
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    output_writer: &mut impl Write,
    parser: &mut vt100::Parser,
    deadline: Instant,
) -> usize {
    let mut relayed = 0;
    loop {
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(chunk) => {
                if relay_chunk(&chunk, output_writer, parser).is_err() {
                    return relayed;
                }
                relayed += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return relayed,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    return relayed;
                }
            }
        }
    }
}

/// The parser's current screen content as plain text, newline-joined - what
/// handler matching (`handlers::find_matching_handler`) is checked against.
pub fn screen_text(parser: &vt100::Parser) -> String {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    (0..rows.get())
        .map(|row| screen.contents_between(row, 0, row, cols.get()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Checks `screen_text` against every active, loaded approval for `role`;
/// on the FIRST exact match not already answered this session (`matched_ids`),
/// writes its `action` to `input_writer` and reports it via `on_answered`.
/// Never matches the same handler id twice, even if its dialog text is
/// still on screen after answering it once.
pub fn check_and_inject(
    handler_list: &[HandlerSpec],
    role: &str,
    now: lane_state::timestamp::Timestamp,
    screen_text: &str,
    matched_ids: &mut HashSet<String>,
    input_writer: &mut impl Write,
    on_answered: &mut impl FnMut(HandlerAnswered),
) -> std::io::Result<()> {
    check_and_inject_with(
        &crate::folder_trust::FsResolver,
        handler_list,
        role,
        now,
        screen_text,
        matched_ids,
        input_writer,
        on_answered,
    )
}

/// [`check_and_inject`] with the filesystem that folder-trust approvals resolve paths on explicit.
#[allow(clippy::too_many_arguments)]
pub fn check_and_inject_with(
    resolver: &dyn crate::folder_trust::FolderResolver,
    handler_list: &[HandlerSpec],
    role: &str,
    now: lane_state::timestamp::Timestamp,
    screen_text: &str,
    matched_ids: &mut HashSet<String>,
    input_writer: &mut impl Write,
    on_answered: &mut impl FnMut(HandlerAnswered),
) -> std::io::Result<()> {
    if let Some((handler, resolved_folder)) =
        handlers::find_matching_handler_resolved(handler_list, role, now, screen_text, resolver)
    {
        if matched_ids.insert(handler.id.clone()) {
            // Validated at load, so this cannot be None; if it ever is, send nothing.
            let bytes = handlers::action_bytes(&handler.action).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "handler {} action is outside the keystroke grammar",
                        handler.id
                    ),
                )
            })?;
            input_writer.write_all(&bytes)?;
            input_writer.flush()?;
            on_answered(HandlerAnswered {
                handler_id: handler.id.clone(),
                matched_screen_text: screen_text.to_string(),
                injected_action: handler.action.clone(),
                resolved_folder,
            });
        }
    }
    Ok(())
}

pub struct HostOutcome {
    pub child_exit_code: Option<u32>,
    /// Helper threads that were still running when the host returned, by
    /// name. Empty means every thread this run started has ended. A name
    /// here is a measured fact about this run, not a guess: see
    /// `LiveFlag`.
    pub abandoned_threads: Vec<&'static str>,
    /// How long after the child exited (`child.wait()` returned) the PTY reader thread ended, or
    /// `None` when it had not ended by the time the host returned. Measured, for issue #228.
    pub reader_end_after_exit: Option<Duration>,
}

/// How long the host waits, after the child has exited and the ConPTY has
/// been closed, for the PTY reader thread to see end-of-file and end.
const READER_END_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the host waits, at teardown, for the outer-input thread to end
/// when its input has reached end-of-file. The thread is not woken by
/// anything the host does, so a run whose child exits first can sample its
/// flag before it has been scheduled; an input that is really blocked (a
/// terminal) stays alive past this and is reported, never hidden.
const INPUT_END_TIMEOUT: Duration = Duration::from_secs(1);

/// True while a helper thread is running. The thread owns the `LiveFlag`
/// and clears the shared bool when the closure returns (or unwinds), so
/// the host can read back whether a thread really ended, instead of
/// assuming it did.
struct LiveFlag(Arc<std::sync::atomic::AtomicBool>);

impl LiveFlag {
    fn new() -> (LiveFlag, Arc<std::sync::atomic::AtomicBool>) {
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        (LiveFlag(Arc::clone(&alive)), alive)
    }
}

impl Drop for LiveFlag {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Stamps the moment the PTY reader thread ends (however it ends) into `ended_ns`.
struct ReaderEndStamp {
    clock: Instant,
    ended_ns: Arc<std::sync::atomic::AtomicU64>,
}

impl Drop for ReaderEndStamp {
    fn drop(&mut self) {
        self.ended_ns.store(
            self.clock.elapsed().as_nanos() as u64 + 1,
            std::sync::atomic::Ordering::SeqCst,
        );
    }
}

/// Tests only (issue #228). On Windows a process spawned while the host is creating its ConPTY inherits
/// the pipe's write end and keeps the reader from seeing end-of-file until THAT process exits
/// (measured: every slow reader end in 80 full-suite runs coincided with a `rustup`/`cargo` child of
/// the test process exiting; most ended in under 100 ms, a few took 0.5 to 5 s). The host holds this
/// lock from before `openpty` until the pipes are cloned; a test that spawns a long-lived child takes
/// it around the spawn. Production never spawns while a host opens its pty, so it has no lock.
#[cfg(test)]
pub(crate) static SPAWN_VS_PTY_OPEN: Mutex<()> = Mutex::new(());

fn is_alive(flag: &Arc<std::sync::atomic::AtomicBool>) -> bool {
    flag.load(std::sync::atomic::Ordering::SeqCst)
}

/// Runs the real relay: opens a ConPTY sized `(cols, rows)`, spawns
/// `child_argv` inside it, and shuttles bytes between it and
/// `outer_input`/`outer_output` (in production, this process's own real
/// stdin/stdout - inherited from `wt.exe`, a real terminal; in a test, a
/// fake pair that stands in for one) until the child exits.
///
/// `on_unhandled`, RESTART-TOOL-DESIGN.md §12.6: fires AT MOST ONCE, the
/// moment the startup window ends, with the screen text captured at that
/// instant - but ONLY if zero handlers matched anywhere during the whole
/// window. A handler that matched (even a different one than whatever's
/// still on screen) means a human or the handler itself is already
/// handling it; capture exists for the case nothing did.
// Tests only: the host's own entry is `run_with_handlers_and_window_watched`; this is the unwatched shape the
// host tests run through (CodeRipper `reachability`: zero callers outside them).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn run_with_handlers(
    role: &str,
    child_argv: &[String],
    handler_list: Vec<HandlerSpec>,
    cols: u16,
    rows: u16,
    outer_input: impl Read + Send + 'static,
    outer_output: impl Write + Send + 'static,
    on_answered: impl FnMut(HandlerAnswered) + Send + 'static,
    on_unhandled: impl FnOnce(String) + Send + 'static,
) -> std::io::Result<HostOutcome> {
    run_with_handlers_and_window(
        role,
        child_argv,
        handler_list,
        cols,
        rows,
        outer_input,
        outer_output,
        on_answered,
        on_unhandled,
        STARTUP_WINDOW,
        |_, _| {},
        |_, _| {},
    )
}

/// The real implementation, with `startup_window` injected so tests don't
/// have to wait out the real 60s `STARTUP_WINDOW` to exercise what happens
/// once it ends (§12.6's unhandled-prompt capture, in particular).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_with_handlers_and_window(
    role: &str,
    child_argv: &[String],
    handler_list: Vec<HandlerSpec>,
    cols: u16,
    rows: u16,
    outer_input: impl Read + Send + 'static,
    outer_output: impl Write + Send + 'static,
    on_answered: impl FnMut(HandlerAnswered) + Send + 'static,
    on_unhandled: impl FnOnce(String) + Send + 'static,
    startup_window: Duration,
    on_match_attempt: impl FnMut(&str, &[handlers::MatchReport]) + Send + 'static,
    on_raw_io: impl Fn(&str, &[u8]) + Send + Sync + 'static,
) -> std::io::Result<HostOutcome> {
    run_with_handlers_and_window_watched(
        role,
        child_argv,
        handler_list,
        cols,
        rows,
        outer_input,
        outer_output,
        on_answered,
        on_unhandled,
        startup_window,
        on_match_attempt,
        on_raw_io,
        None,
        None,
    )
}

/// The implementation, with a `watch` hub (`crate::watch`, `attach --watch`): when present, the relay
/// loop answers watchers' requests with the current screen and publishes every chunk to them. It
/// reads nothing from them. `None` (every test but those that gate the window) is exactly the relay as it was.
///
/// `window_gate` (issue #117): `None` starts the startup window at spawn, as production does.
/// `Some(flag)` holds the window's clock until the flag is set, then runs it from that instant, so
/// a test can start it when the child's first output has really arrived instead of guessing how
/// long a loaded runner takes to start the child.
#[allow(clippy::too_many_arguments)]
fn run_with_handlers_and_window_watched(
    role: &str,
    child_argv: &[String],
    handler_list: Vec<HandlerSpec>,
    cols: u16,
    rows: u16,
    mut outer_input: impl Read + Send + 'static,
    mut outer_output: impl Write + Send + 'static,
    on_answered: impl FnMut(HandlerAnswered) + Send + 'static,
    on_unhandled: impl FnOnce(String) + Send + 'static,
    startup_window: Duration,
    mut on_match_attempt: impl FnMut(&str, &[handlers::MatchReport]) + Send + 'static,
    on_raw_io: impl Fn(&str, &[u8]) + Send + Sync + 'static,
    watch: Option<Arc<crate::watch::WatchHub>>,
    window_gate: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> std::io::Result<HostOutcome> {
    let on_raw_io = Arc::new(on_raw_io);
    #[cfg(test)]
    let pty_open_guard = SPAWN_VS_PTY_OPEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(std::io::Error::other)?;

    // ⚠️ ROOT CAUSE, PM finding 2026-09-19 (real-restart-04:39:56Z): with no
    // explicit `.cwd()`, `portable_pty::CommandBuilder` falls back to
    // `USERPROFILE`, NOT this process's own current directory - confirmed by
    // reading `cmdbuilder.rs`'s own `current_directory()`. `claude.exe` was
    // spawned in the WRONG directory entirely, which is why
    // `find_claude_process_in`'s cwd match silently never found it (`alive`
    // came back false at the 20s check, producing `SessionNeverProcessedPrompt`
    // instead of `AwaitingConfirmation`, even though the process was genuinely
    // alive). `lane-restart host` itself is always launched via
    // `wt.exe -d <cwd> lane-restart host ...` (`spawn_relaunch`), so ITS OWN
    // `current_dir()` IS the correct target directory - explicitly propagated
    // here rather than trusted to any implicit inheritance.
    let mut cmd = CommandBuilder::new(&child_argv[0]);
    cmd.args(&child_argv[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(std::io::Error::other)?;
    drop(pair.slave);

    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .map_err(std::io::Error::other)?;
    let pty_writer = Arc::new(Mutex::new(
        pair.master.take_writer().map_err(std::io::Error::other)?,
    ));
    #[cfg(test)]
    drop(pty_open_guard);

    // outer input -> child, transparent, for the whole session's life.
    // ⚠️ THIS is the thread that carries WT's own real-terminal CPR reply
    // (and every other keystroke) into the child - PM finding, 2026-09-19
    // (real-restart-2): if this thread isn't running, starts late, or
    // never receives anything, the child can sit blocked on its own CPR
    // forever with a blank screen. `on_raw_io` records every read here so
    // a stuck run shows whether this thread ever received a single byte.
    let (input_flag, input_alive) = LiveFlag::new();
    {
        let pty_writer = Arc::clone(&pty_writer);
        let on_raw_io = Arc::clone(&on_raw_io);
        std::thread::spawn(move || {
            let _alive = input_flag;
            let mut buf = [0u8; 4096];
            loop {
                match outer_input.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        on_raw_io("outer_stdin->child", &buf[..n]);
                        let mut w = pty_writer.lock().unwrap();
                        if w.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        let _ = w.flush();
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // Best-effort resize forwarding: poll our own inherited console size
    // and forward changes to the ConPTY. Never fatal if it fails - the
    // relay itself doesn't depend on it.
    // ⚠️ This thread OWNS the pty master, so the ConPTY stays open for as
    // long as it runs. It ends when the host drops `stop_resize` (or sends
    // on it), which is what lets the master close - and the reader below
    // see end-of-file - once the child is gone.
    let (stop_resize, stop_resize_rx) = std::sync::mpsc::channel::<()>();
    let (resize_flag, resize_alive) = LiveFlag::new();
    // Set by the relay loop to ask for a repaint: this thread owns the master, so it does the
    // resize nudge (one column wider and back), which makes ConPTY repaint the whole screen.
    let nudge = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Set once per start by the relay loop: resize the pty to `REDRAW_SIZE` and leave it there.
    let redraw = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resize_thread = {
        let master_for_resize = pair.master;
        let mut last = (cols, rows);
        // The degenerate console size already logged (noted once, not every poll).
        let mut noted_degenerate: Option<(u16, u16)> = None;
        let nudge = Arc::clone(&nudge);
        let redraw = Arc::clone(&redraw);
        let on_raw_io = Arc::clone(&on_raw_io);
        std::thread::spawn(move || {
            let _alive = resize_flag;
            // Every size this thread sets goes to the host log (2026-11-26 PM row: launcher starts
            // stall on a footer-only dialog in a 2-row or ~10-column pty; where the size came from
            // was not recorded). A failed resize is logged too.
            let note = |what: &str, c: u16, r: u16, res: &std::io::Result<()>| {
                let outcome = match res {
                    Ok(()) => "ok".to_string(),
                    Err(e) => format!("FAILED {e}"),
                };
                on_raw_io(
                    "host-note",
                    format!("PTY SIZE {what}: {c}x{r} (cols x rows) {outcome}").as_bytes(),
                );
            };
            // Ends on a stop message or when the host drops the sender.
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stop_resize_rx.recv_timeout(Duration::from_millis(500))
            {
                if redraw.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let (c, r) = REDRAW_SIZE;
                    let res = master_for_resize
                        .resize(PtySize {
                            rows: r,
                            cols: c,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .map_err(std::io::Error::other);
                    note("REDRAW", c, r, &res);
                    if res.is_ok() {
                        last = (c, r);
                    }
                }
                if nudge.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let (c, r) = last;
                    let size = |cols| PtySize {
                        rows: r,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    };
                    let wider = c.saturating_add(1);
                    let res = master_for_resize
                        .resize(size(wider))
                        .map_err(std::io::Error::other);
                    note("nudge wider", wider, r, &res);
                    std::thread::sleep(Duration::from_millis(100));
                    let res = master_for_resize
                        .resize(size(c))
                        .map_err(std::io::Error::other);
                    note("nudge back", c, r, &res);
                }
                if let Some(polled) = current_console_size() {
                    let action = poll_action(last, polled, &mut noted_degenerate);
                    if action == PollAction::NoteDegenerate {
                        on_raw_io(
                            "host-note",
                            format!(
                                "PTY SIZE ignored a degenerate console size {}x{} (cols x rows); the pty stays {}x{} (noted once while the console stays this size)",
                                polled.0, polled.1, last.0, last.1
                            )
                            .as_bytes(),
                        );
                    }
                    if let PollAction::Forward((c, r)) = action {
                        let res = master_for_resize
                            .resize(PtySize {
                                rows: r,
                                cols: c,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .map_err(std::io::Error::other);
                        note(
                            &format!("follows the host's own console (was {}x{})", last.0, last.1),
                            c,
                            r,
                            &res,
                        );
                        last = (c, r);
                    }
                }
            }
        })
    };

    // ⚠️ THE CPR-BLOCKING INVESTIGATION'S OTHER FINDING (2026-09-18,
    // scratch-probe iteration 4): ConPTY can keep its own internal
    // reference to the pipe's write end alive even after the hosted child
    // exits, so a blocking `read` in THIS thread can hang forever past
    // exit - a plain "loop until read returns 0" never terminates in that
    // case. Fixed the same way the probe was: the blocking read lives on
    // its own thread, sending chunks over a channel; this thread polls
    // that channel with a short timeout and separately checks
    // `child.try_wait()` for the real exit signal. Once the child is gone
    // the pty master is dropped (see the resize thread), which closes the
    // ConPTY and ends the reader's `ReadFile`; the host waits a bounded
    // `READER_END_TIMEOUT` for that and reports (never hides) a reader that
    // is still blocked, in `HostOutcome::abandoned_threads`.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (reader_flag, reader_alive) = LiveFlag::new();
    // When the reader ended, in nanoseconds since `clock` plus one (0: not yet). Written by the reader
    // just before its flag clears; read by the host after the child exit, for `reader_end_after_exit`.
    let clock = Instant::now();
    let reader_ended_ns = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let on_raw_io = Arc::clone(&on_raw_io);
        let reader_ended_ns = Arc::clone(&reader_ended_ns);
        std::thread::spawn(move || {
            let _alive = reader_flag;
            let _stamp = ReaderEndStamp {
                clock,
                ended_ns: reader_ended_ns,
            };
            let mut buf = [0u8; 4096];
            loop {
                match pty_reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        on_raw_io("child->relay", &buf[..n]);
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let mut on_answered = on_answered;
    let mut on_unhandled = Some(on_unhandled);
    let mut unhandled_thread: Option<std::thread::JoinHandle<()>> = None;
    let mut parser = vt100::Parser::new(
        std::num::NonZeroU16::new(rows).unwrap_or(std::num::NonZeroU16::MIN),
        std::num::NonZeroU16::new(cols).unwrap_or(std::num::NonZeroU16::MIN),
        0,
    );
    let mut matched_ids: HashSet<String> = HashSet::new();
    let mut last_logged_screen: Option<String> = None;
    let mut footer_since: Option<Instant> = None;
    let mut repaints = 0u32;
    // The window runs from spawn unless a test gated it (see `window_gate`).
    let mut window_started = window_gate.is_none().then(Instant::now);
    loop {
        if window_started.is_none()
            && window_gate
                .as_ref()
                .is_some_and(|gate| gate.load(std::sync::atomic::Ordering::SeqCst))
        {
            window_started = Some(Instant::now());
        }
        let still_in_startup_window = window_started.is_none_or(|t| t.elapsed() < startup_window);
        if !still_in_startup_window {
            if let Some(capture) = on_unhandled.take() {
                if matched_ids.is_empty() {
                    // Issue #96 (a): `capture` sends the PM ask over the
                    // claude-peers broker, so it runs on its own thread - a
                    // stalled broker must never pause the terminal. The
                    // screen text is taken HERE, at the window's end.
                    let text = screen_text(&parser);
                    unhandled_thread = Some(std::thread::spawn(move || capture(text)));
                }
            }
        }
        // Watchers ask between chunks, so the screen they are given is exactly as of the chunks
        // relayed so far.
        if let Some(hub) = &watch {
            hub.service(|| crate::watch::snapshot_of(&parser));
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                relay_chunk(&chunk, &mut outer_output, &mut parser)?;
                on_raw_io("relay->outer_stdout", &chunk);
                if let Some(hub) = &watch {
                    hub.publish(&chunk);
                }
                if still_in_startup_window {
                    let text = screen_text(&parser);
                    if Some(text.as_str()) != last_logged_screen.as_deref() {
                        let reports: Vec<handlers::MatchReport> = handler_list
                            .iter()
                            .map(|h| {
                                handlers::match_report(
                                    h,
                                    role,
                                    lane_state::timestamp::Timestamp::now(),
                                    &text,
                                )
                            })
                            .collect();
                        on_match_attempt(&text, &reports);
                        last_logged_screen = Some(text.clone());
                    }
                    let mut writer = pty_writer.lock().unwrap();
                    check_and_inject(
                        &handler_list,
                        role,
                        lane_state::timestamp::Timestamp::now(),
                        &text,
                        &mut matched_ids,
                        &mut *writer,
                        &mut on_answered,
                    )?;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                // 2026-11-22: four dialogs stayed as "Enter to confirm / Esc to cancel" with no
                // body, so no handler could match and nothing re-read the screen (handlers run
                // only when output arrives). A footer alone for REPAINT_AFTER asks ConPTY to
                // repaint; the repaint is output, so the handlers run on it as usual.
                if still_in_startup_window
                    && matched_ids.is_empty()
                    && repaints < MAX_REPAINTS
                    && footer_since.is_none_or(|t| t.elapsed() >= REPAINT_AFTER)
                {
                    let text = screen_text(&parser);
                    if footer_only(&text) {
                        if footer_since.is_some() {
                            repaints += 1;
                            nudge.store(true, std::sync::atomic::Ordering::SeqCst);
                            on_raw_io(
                                "host-note",
                                format!("REPAINT {repaints}/{MAX_REPAINTS}: only a dialog footer on screen for {} s, resize nudge sent; screen {text:?}", REPAINT_AFTER.as_secs()).as_bytes(),
                            );
                            if repaints == REDRAW_AFTER_REPAINTS {
                                redraw.store(true, std::sync::atomic::Ordering::SeqCst);
                                on_raw_io("host-note", redraw_request_note(repaints).as_bytes());
                            }
                        }
                        footer_since = Some(Instant::now());
                    } else {
                        footer_since = None;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait().ok();
    let exited_ns = clock.elapsed().as_nanos() as u64;

    // Teardown. The resize thread owns the pty master: stop it, and its
    // exit closes the ConPTY, which is what ends the reader's blocking
    // read. Then give the reader a bounded time to say so; whatever is
    // still running after that is reported, never silently left.
    drop(stop_resize);
    let _ = resize_thread.join();
    // Whatever the reader still delivers while the ConPTY closes is the child's last output
    // (a ConPTY may flush its final frame only when it is closed), so it is relayed here, not
    // left in a channel nobody reads any more: this wait is also the drain.
    let _ = drain_tail(
        &rx,
        &mut outer_output,
        &mut parser,
        Instant::now() + READER_END_TIMEOUT,
    );
    // Joined so the process can't exit mid-ask. The relay has stopped by
    // now, so waiting here stalls nothing.
    if let Some(t) = unhandled_thread {
        let _ = t.join();
    }
    let input_deadline = Instant::now() + INPUT_END_TIMEOUT;
    while is_alive(&input_alive) && Instant::now() < input_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut abandoned_threads = Vec::new();
    if is_alive(&reader_alive) {
        abandoned_threads.push("pty-reader");
    }
    if is_alive(&input_alive) {
        abandoned_threads.push("outer-input");
    }
    if is_alive(&resize_alive) {
        abandoned_threads.push("resize");
    }
    let ended = reader_ended_ns.load(std::sync::atomic::Ordering::SeqCst);
    let reader_end_after_exit =
        (ended != 0).then(|| Duration::from_nanos((ended - 1).saturating_sub(exited_ns)));
    Ok(HostOutcome {
        child_exit_code: status.and_then(|s| s.exit_code().into()),
        abandoned_threads,
        reader_end_after_exit,
    })
}

/// `.lane-state/host-<role>-<pid>.log` - PM finding, 2026-09-19: a real
/// restart left NOTHING to explain a handler that should have matched but
/// didn't, so this diagnostic log exists purely to make the NEXT run
/// explain itself: the screen text at each real change, the per-anchor and
/// per-field match result for every loaded approval, and any injection
/// with the exact bytes sent.
fn host_log_path(role: &str) -> std::path::PathBuf {
    crate::state_dir().join(format!("host-{role}-{}.log", std::process::id()))
}

/// A host log is rotated when it reaches this size: the file moves to `<name>.1` (replacing the
/// previous one), so at most two files per host, each at most this size plus one write.
pub const HOST_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// A host log older than this is deleted by [`prune_host_logs`].
pub const HOST_LOG_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// The environment variable that turns the raw terminal stream into the host log. Off by default
/// (2026-11-21, Defender recording: every redraw wrote two lines and every write was scanned; the
/// host logs were the biggest single scan cost on the machine).
pub const RAW_LOG_VAR: &str = "LANE_HOST_RAW_LOG";

/// The value of [`RAW_LOG_VAR`] a launcher start sets: the raw stream for the startup window only.
pub const RAW_LOG_STARTUP: &str = "startup";
/// A startup capture stops after this long ...
const RAW_STARTUP_WINDOW: Duration = Duration::from_secs(120);
/// ... or after this many bytes of log text, whichever comes first.
const RAW_STARTUP_MAX_BYTES: usize = 1024 * 1024;

/// How often a raw-stream log is flushed to disk, when it is on.
const RAW_LOG_FLUSH: Duration = Duration::from_secs(3);

/// `1` or `true` (any case) turns the raw stream on; anything else, or unset, leaves it off.
pub fn raw_log_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
}

/// `startup` asks for the bounded raw stream (a launcher start; see [`RAW_STARTUP_WINDOW`]).
pub fn raw_log_startup(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case(RAW_LOG_STARTUP))
}

/// Best-effort append - never fatal to the relay that's still running. Rotates first when the file
/// has reached [`HOST_LOG_MAX_BYTES`].
fn append_host_log(path: &std::path::Path, text: &str) {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_full(path, HOST_LOG_MAX_BYTES);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{text}");
    }
}

/// Moves `path` to `<path>.1` when it is at least `max` bytes, replacing a previous `.1`.
fn rotate_if_full(path: &std::path::Path, max: u64) {
    let full = std::fs::metadata(path).is_ok_and(|m| m.len() >= max);
    if full {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let old = std::path::PathBuf::from(old);
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(path, &old);
    }
}

/// The raw terminal stream, buffered: lines are held and written in one append every
/// [`RAW_LOG_FLUSH`] (or at 64 KiB, or when the host ends), not one write per chunk. Only exists
/// when [`RAW_LOG_VAR`] is on.
struct RawLog {
    path: std::path::PathBuf,
    pending: Mutex<(String, Instant)>,
    flush_every: Duration,
    /// `Some` for a startup capture: it stops at the window or the byte cap and says so once.
    limit: Option<RawLimit>,
}

struct RawLimit {
    started: Instant,
    window: Duration,
    max_bytes: usize,
    /// Bytes of log text accepted so far; `None` once the capture has stopped.
    taken: Mutex<Option<usize>>,
}

impl RawLog {
    fn new(path: std::path::PathBuf, flush_every: Duration) -> Self {
        Self {
            path,
            pending: Mutex::new((String::new(), Instant::now())),
            flush_every,
            limit: None,
        }
    }

    /// A capture of the startup window only: [`RAW_STARTUP_WINDOW`] and [`RAW_STARTUP_MAX_BYTES`].
    fn startup(path: std::path::PathBuf, flush_every: Duration) -> Self {
        Self::bounded(path, flush_every, RAW_STARTUP_WINDOW, RAW_STARTUP_MAX_BYTES)
    }

    fn bounded(
        path: std::path::PathBuf,
        flush_every: Duration,
        window: Duration,
        max_bytes: usize,
    ) -> Self {
        let mut r = Self::new(path, flush_every);
        r.limit = Some(RawLimit {
            started: Instant::now(),
            window,
            max_bytes,
            taken: Mutex::new(Some(0)),
        });
        r
    }

    fn push(&self, direction: &str, bytes: &[u8]) {
        let line = format!(
            "[{}] {direction} n={} {}\n",
            lane_state::timestamp::Timestamp::now().to_rfc3339(),
            bytes.len(),
            escape_bytes(bytes)
        );
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = &self.limit {
            let mut taken = l.taken.lock().unwrap_or_else(|e| e.into_inner());
            let Some(n) = *taken else { return };
            if l.started.elapsed() >= l.window || n + line.len() > l.max_bytes {
                *taken = None;
                p.0.push_str(&format!(
                    "[{}] RAW-STREAM stopped: startup capture is limited to {} s and {} bytes\n",
                    lane_state::timestamp::Timestamp::now().to_rfc3339(),
                    l.window.as_secs(),
                    l.max_bytes
                ));
                let text = std::mem::take(&mut p.0);
                drop(p);
                self.write(&text);
                return;
            }
            *taken = Some(n + line.len());
        }
        p.0.push_str(&line);
        if p.1.elapsed() >= self.flush_every || p.0.len() >= 64 * 1024 {
            let text = std::mem::take(&mut p.0);
            p.1 = Instant::now();
            drop(p);
            self.write(&text);
        }
    }

    fn write(&self, text: &str) {
        use std::io::Write as _;
        if text.is_empty() {
            return;
        }
        rotate_if_full(&self.path, HOST_LOG_MAX_BYTES);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

impl Drop for RawLog {
    fn drop(&mut self) {
        let text = std::mem::take(&mut self.pending.lock().unwrap_or_else(|e| e.into_inner()).0);
        self.write(&text);
    }
}

/// `(role, pid)` of a host log file name: `host-<role>-<pid>.log` or `.log.1`.
fn host_log_name_parts(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_prefix("host-")?;
    let stem = stem
        .strip_suffix(".log.1")
        .or_else(|| stem.strip_suffix(".log"))?;
    let (role, pid) = stem.rsplit_once('-')?;
    (!role.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then_some((role, pid))
}

/// Deletes `host-<role>-<pid>.log` (and `.log.1`) files in `dir` last written more than `max_age`
/// before `now`; with `role`, only that role's. Returns how many were deleted. Only files that
/// match the host log name exactly are touched.
pub fn prune_host_logs(
    dir: &std::path::Path,
    max_age: Duration,
    now: std::time::SystemTime,
    role: Option<&str>,
) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut deleted = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((r, _)) = host_log_name_parts(&name) else {
            continue;
        };
        if role.is_some_and(|want| want != r) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if old && std::fs::remove_file(e.path()).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// Convenience entry point for real production use: loads the user's
/// approvals (§12.3) BEFORE spawning the child - a dialog that renders
/// before its handlers arrive would never be re-checked on a static screen
/// - and wires this process's OWN real stdin/stdout as the outer relay ends.
pub fn run(role: &str, child_argv: &[String]) -> std::io::Result<HostOutcome> {
    #[cfg(unix)]
    detach_session_if_headless();
    let log_path = host_log_path(role);
    let started_at = lane_state::timestamp::Timestamp::now();
    let _ = prune_host_logs(
        &crate::state_dir(),
        HOST_LOG_MAX_AGE,
        std::time::SystemTime::now(),
        Some(role),
    );

    eprintln!("lane-restart host: loading approvals...");
    let (config, loaded) = match crate::paths::home_dir() {
        Some(home) => approvals::load_configured(&home, &approvals::default_reader(&home)),
        None => (
            None,
            approvals::Loaded {
                error: Some("no home directory (USERPROFILE/HOME unset)".into()),
                ..approvals::Loaded::default()
            },
        ),
    };
    let approvals_summary = loaded.summary();
    eprintln!("lane-restart host: approvals: {approvals_summary}");
    append_host_log(
        &log_path,
        &format!(
            "[{}] APPROVALS {approvals_summary}",
            started_at.to_rfc3339()
        ),
    );
    if loaded.handlers.is_empty() {
        // Startup self-check: a lane relaunched with no active approval waits for a person at its
        // first dialog (2026-10-22: two lanes did, because GitHub was slow). Say so at boot.
        eprintln!(
            "lane-restart host: WARNING: no approvals are active; a startup dialog will wait for a person"
        );
        append_host_log(
            &log_path,
            "APPROVALS-WARNING no handler is active: every startup dialog goes unanswered",
        );
    }
    for (h, hash) in &loaded.handlers {
        append_host_log(&log_path, &format!("  active {} {hash}", h.id));
    }
    let raw_var = std::env::var(RAW_LOG_VAR).ok();
    let raw_log = if raw_log_enabled(raw_var.as_deref()) {
        Some(RawLog::new(log_path.clone(), RAW_LOG_FLUSH))
    } else if raw_log_startup(raw_var.as_deref()) {
        Some(RawLog::startup(log_path.clone(), RAW_LOG_FLUSH))
    } else {
        None
    };
    append_host_log(
        &log_path,
        &format!(
            "[{}] RAW-STREAM {}",
            started_at.to_rfc3339(),
            if raw_log.is_some() {
                if raw_log_startup(raw_var.as_deref()) {
                    format!(
                        "on (launcher start): first {} s, at most {} bytes, flushed every 3 s",
                        RAW_STARTUP_WINDOW.as_secs(),
                        RAW_STARTUP_MAX_BYTES
                    )
                } else {
                    format!("on ({RAW_LOG_VAR}): buffered, flushed every 3 s")
                }
            } else {
                format!("off (set {RAW_LOG_VAR}=1 to record it)")
            }
        ),
    );
    for (name, reason) in &loaded.refused {
        append_host_log(&log_path, &format!("  REFUSED {name}: {reason}"));
    }
    let handler_list = loaded.specs();

    // ⚠️ Kept alive for the relay's ENTIRE scope - restored on every exit
    // path via `Drop`, including a panic unwind.
    #[cfg(any(windows, unix))]
    let _raw_console_guard = RawConsoleGuard::enable();

    let ((cols, rows), size_source) = creation_size(current_console_size());
    append_host_log(
        &log_path,
        &format!(
            "[{}] PTY SIZE at creation: {cols}x{rows} (cols x rows), source: {size_source}",
            lane_state::timestamp::Timestamp::now().to_rfc3339()
        ),
    );
    let log_path_for_answered = log_path.clone();
    let role_for_answered = role.to_string();
    let redeliver_target = synapse_target(role, child_argv);
    let notify_for_answered = (
        config
            .as_ref()
            .map_or("pm".to_string(), |c| c.notify_role.clone()),
        config
            .as_ref()
            .map_or(notify::Transport::Broker, |c| c.notify_transport),
    );
    let log_path_for_attempts = log_path.clone();
    let log_path_for_notes = log_path.clone();
    let log_path_for_unhandled = log_path.clone();
    let role_for_unhandled = role.to_string();
    let on_unhandled = move |screen: String| {
        handle_unhandled(
            &role_for_unhandled,
            started_at,
            config.as_ref(),
            &approvals_summary,
            &log_path_for_unhandled,
            screen,
        )
    };
    // `attach --watch`: a read-only socket (`crate::watch`). Best-effort: a host that cannot bind it
    // still runs the lane; it only cannot be watched. Stopped, and the socket removed, when this
    // function returns.
    let watch_hub = Arc::new(crate::watch::WatchHub::new());
    #[cfg(unix)]
    let _watch_server = {
        let path = crate::watch::socket_path(&crate::state_dir(), role);
        match crate::watch::serve(Arc::clone(&watch_hub), path) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("lane-restart host: watch socket not served: {e}");
                None
            }
        }
    };
    run_with_handlers_and_window_watched(
        role,
        child_argv,
        handler_list,
        cols,
        rows,
        std::io::stdin(),
        std::io::stdout(),
        move |answered| {
            eprintln!(
                "lane-restart host: answered dialog via handler {:?}",
                answered.handler_id
            );
            append_host_log(
                &log_path_for_answered,
                &format!(
                    "[{}] INJECTED handler={} bytes={:?} screen={:?}",
                    lane_state::timestamp::Timestamp::now().to_rfc3339(),
                    answered.handler_id,
                    answered.injected_action,
                    answered.matched_screen_text
                ),
            );
            if let Some(note) =
                request_redeliver_after_answer(&answered.handler_id, redeliver_target.as_ref())
            {
                append_host_log(
                    &log_path_for_answered,
                    &format!(
                        "[{}] {note}",
                        lane_state::timestamp::Timestamp::now().to_rfc3339()
                    ),
                );
            }
            if let Some(folder) = &answered.resolved_folder {
                record_folder_trust_answer(
                    &crate::state_dir(),
                    &role_for_answered,
                    &answered.handler_id,
                    folder,
                    &notify_for_answered.0,
                    notify_for_answered.1,
                    &log_path_for_answered,
                );
            }
        },
        on_unhandled,
        STARTUP_WINDOW,
        move |screen, reports| {
            append_host_log(
                &log_path_for_attempts,
                &format!(
                    "[{}] SCREEN CHANGED {:?}",
                    lane_state::timestamp::Timestamp::now().to_rfc3339(),
                    screen
                ),
            );
            for r in reports {
                append_host_log(
                    &log_path_for_attempts,
                    &format!(
                        "  handler={} active={} matched={} anchors={:?} fields={:?} lines={:?}",
                        r.handler_id, r.active, r.matched, r.anchors, r.fields, r.lines
                    ),
                );
                if let Some((shown, ok)) = &r.folder {
                    append_host_log(
                        &log_path_for_attempts,
                        &format!(
                            "  handler={} folder={shown:?} trusted_under_prefix={ok}",
                            r.handler_id
                        ),
                    );
                }
            }
        },
        move |direction, bytes| {
            // The relay's own notes (a repaint request) always reach the host log.
            if direction == "host-note" {
                append_host_log(
                    &log_path_for_notes,
                    &format!(
                        "[{}] {}",
                        lane_state::timestamp::Timestamp::now().to_rfc3339(),
                        String::from_utf8_lossy(bytes)
                    ),
                );
            } else if let Some(raw) = &raw_log {
                raw.push(direction, bytes);
            }
        },
        Some(watch_hub),
        None,
    )
}

/// One line of `folder-trust-answers.log`: time, lane, handler, resolved folder.
fn folder_trust_record(now: &str, role: &str, handler_id: &str, folder: &str) -> String {
    serde_json::json!({"time": now, "lane": role, "handler": handler_id, "folder": folder})
        .to_string()
}

/// The one-line notice sent to the notify role (compact [NOTICE] format, MESSAGE-FORMATS.md).
fn folder_trust_notice(now: &str, role: &str, handler_id: &str, folder: &str) -> String {
    format!(
        "[NOTICE] topic=folder-trust-auto-answer affects={role} when={now} action=none path={folder} handler={handler_id}"
    )
}

/// Every folder-trust prefix auto-answer is recorded where the PM reads it
/// (`<state dir>/folder-trust-answers.log`, one JSON object per line) and announced to the notify
/// role in one line (board 244: the answer is blanket, so each use is visible). Never takes down the
/// relay: failures are logged to the host log and the announcement is best-effort on its own thread
/// (the broker call can be slow).
fn record_folder_trust_answer(
    state_dir: &std::path::Path,
    role: &str,
    handler_id: &str,
    folder: &str,
    notify_role: &str,
    transport: notify::Transport,
    host_log: &std::path::Path,
) {
    let now = lane_state::timestamp::Timestamp::now().to_rfc3339();
    let log = state_dir.join("folder-trust-answers.log");
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut f| writeln!(f, "{}", folder_trust_record(&now, role, handler_id, folder)))
    {
        append_host_log(
            host_log,
            &format!(
                "[{now}] FOLDER-TRUST log {} not written: {e}",
                log.display()
            ),
        );
    }
    let text = folder_trust_notice(&now, role, handler_id, folder);
    let (state_dir, notify_role, host_log) = (
        state_dir.to_path_buf(),
        notify_role.to_string(),
        host_log.to_path_buf(),
    );
    std::thread::spawn(move || {
        let sent = match transport {
            notify::Transport::Broker => {
                notify::notify_role(notify::broker_addr(), &state_dir, &notify_role, &text)
                    .map(|_| ())
            }
            notify::Transport::Synapse => {
                notify::enqueue(&state_dir, &notify_role, &text).map(|_| ())
            }
        };
        let line = match sent {
            Ok(()) => format!("[{now}] FOLDER-TRUST notice sent to {notify_role}"),
            Err(e) => format!("[{now}] FOLDER-TRUST notice to {notify_role} FAILED: {e}"),
        };
        append_host_log(&host_log, &line);
    });
}

/// PM finding, 2026-09-19 (real-restart-2): "first ~200 bytes escaped" -
/// lossy UTF-8 plus Rust's own `Debug` escaping (`\n`, `\r`, `\u{1b}`, …) is
/// what every other diagnostic string in this module already uses for
/// screen text, kept consistent here for raw I/O too.
fn escape_bytes(bytes: &[u8]) -> String {
    let take = bytes.len().min(200);
    format!("{:?}", String::from_utf8_lossy(&bytes[..take]))
}

/// RESTART-TOOL-DESIGN.md §12.6: the startup window ended and no handler
/// matched. If the lane has since shown real progress there was no dialog
/// to answer, and nothing happens. Otherwise: capture the screen, then tell
/// the notify role's lane directly (§12.6's ask flow) - without anyone
/// having to watch any stdout. Every step is logged; none can take down
/// the relay.
fn handle_unhandled(
    role: &str,
    started_at: lane_state::timestamp::Timestamp,
    config: Option<&approvals::Config>,
    approvals_summary: &str,
    log_path: &std::path::Path,
    screen: String,
) {
    let state_dir_buf = crate::state_dir();
    let state_dir = state_dir_buf.as_path();
    let now = lane_state::timestamp::Timestamp::now().to_rfc3339();
    if notify::lane_progressed_since(state_dir, role, started_at) {
        append_host_log(
            log_path,
            &format!("[{now}] UNHANDLED-CHECK lane progressed; no dialog waiting, no ask sent"),
        );
        return;
    }
    let capture = capture_unhandled_prompt(&screen);
    let notify_role = config.map_or("pm", |c| c.notify_role.as_str());
    let ask = notify::Ask {
        role,
        notify_role,
        host_pid: std::process::id(),
        capture_path: capture.as_deref(),
        approvals_summary,
        approvals_source: config.map(|c| (c.approvals.repo.as_str(), c.approvals.path.as_str())),
        screen_text: &screen,
    };
    let transport = config.map_or(notify::Transport::Broker, |c| c.notify_transport);
    let sent = match transport {
        notify::Transport::Broker => {
            notify::notify_role(notify::broker_addr(), state_dir, notify_role, &ask.text())
                .map(|ids| format!("[{now}] ASK SENT to {notify_role} peers {ids:?}"))
        }
        // Queued, not sent: the launcher service relays it as `agentlife-launcher`.
        notify::Transport::Synapse => notify::enqueue(state_dir, notify_role, &ask.text())
            .map(|p| format!("[{now}] ASK QUEUED for {notify_role} at {}", p.display())),
    };
    let line = match sent {
        Ok(line) => line,
        Err(e) => {
            eprintln!("lane-restart host: could not notify the {notify_role} lane: {e}");
            format!("[{now}] ASK FAILED to {notify_role}: {e}")
        }
    };
    append_host_log(log_path, &line);
}

/// RESTART-TOOL-DESIGN.md §12.6: writes the captured screen text verbatim
/// to `.lane-state/unhandled-prompts/<timestamp>.txt` - the fixed,
/// portfolio-wide state directory `main.rs`'s own `state_dir()` uses, not
/// configurable here either, for the same reason: a caller-supplied
/// location would defeat the point of every lane and the PM agreeing on
/// one place to look. Best-effort: a write failure is reported to stderr,
/// never allowed to take down the relay that's still running.
fn capture_unhandled_prompt(screen_text: &str) -> Option<std::path::PathBuf> {
    let dir = crate::state_dir().join("unhandled-prompts");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("lane-restart host: could not create {dir:?}: {e}");
        return None;
    }
    let path = dir.join(
        format!(
            "{}.txt",
            lane_state::timestamp::Timestamp::now().to_rfc3339()
        )
        .replace(':', "-"),
    );
    match std::fs::write(&path, screen_text) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!("lane-restart host: could not write {path:?}: {e}");
            None
        }
    }
}

#[cfg(windows)]
pub fn current_console_size() -> Option<(u16, u16)> {
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let cols = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16;
        let rows = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16;
        Some((cols, rows))
    }
}

/// The size of the tty on `fd`, or `None` when it is not one (or reports 0x0, as a pty nobody has
/// sized does).
#[cfg(unix)]
fn tty_size_of(fd: std::os::fd::BorrowedFd<'_>) -> Option<(u16, u16)> {
    let w = rustix::termios::tcgetwinsize(fd).ok()?;
    (w.ws_col > 0 && w.ws_row > 0).then_some((w.ws_col, w.ws_row))
}

#[cfg(unix)]
pub fn current_console_size() -> Option<(u16, u16)> {
    use std::os::fd::AsFd;
    tty_size_of(std::io::stdout().as_fd())
}

#[cfg(not(any(windows, unix)))]
pub fn current_console_size() -> Option<(u16, u16)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The child a host test runs: the Windows `cmd.exe` argv as written, or `sh -c <script>` on
    /// unix (`docs/PORT-LINUX-PLAN.md`, slice P4).
    fn platform_child(windows: Vec<String>, unix_script: &str) -> Vec<String> {
        if cfg!(windows) {
            windows
        } else {
            vec!["sh".to_string(), "-c".to_string(), unix_script.to_string()]
        }
    }

    // -- host log volume (2026-11-21, Defender recording) -------------------- //

    #[test]
    fn the_raw_stream_is_off_unless_the_flag_says_one_or_true() {
        for off in [None, Some(""), Some("0"), Some("yes"), Some("false")] {
            assert!(!raw_log_enabled(off), "{off:?}");
        }
        for on in [Some("1"), Some(" 1 "), Some("true"), Some("TRUE")] {
            assert!(raw_log_enabled(on), "{on:?}");
        }
    }

    #[test]
    fn a_raw_log_writes_once_per_interval_not_once_per_chunk_and_flushes_at_the_end() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("host-x-1.log");
        // an interval that does not elapse during the test: 500 chunks, nothing on disk yet
        let raw = RawLog::new(path.clone(), Duration::from_secs(3600));
        for i in 0..500 {
            raw.push("child->relay", format!("chunk {i}").as_bytes());
        }
        assert!(!path.exists(), "buffered, not written per chunk");
        drop(raw);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            500,
            "every chunk is there after the flush"
        );
        // control: an interval of zero writes as it goes
        let eager = RawLog::new(d.path().join("host-y-1.log"), Duration::ZERO);
        eager.push("child->relay", b"a");
        assert!(d.path().join("host-y-1.log").exists());
    }

    #[test]
    fn startup_raw_capture_stops_at_the_byte_cap_and_the_window_and_says_so_once() {
        assert!(raw_log_startup(Some("startup")) && raw_log_startup(Some(" Startup ")));
        // control: the full-capture values and unset are not the startup mode
        for no in [None, Some("1"), Some("true"), Some("")] {
            assert!(!raw_log_startup(no), "{no:?}");
        }
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("host-x-1.log");
        let raw = RawLog::bounded(path.clone(), Duration::ZERO, Duration::from_secs(3600), 400);
        for _ in 0..50 {
            raw.push("child->relay", b"0123456789");
        }
        drop(raw);
        let text = std::fs::read_to_string(&path).unwrap();
        let chunks = text.lines().filter(|l| l.contains("child->relay")).count();
        assert!((1..50).contains(&chunks), "capped, got {chunks}");
        assert_eq!(text.matches("RAW-STREAM stopped").count(), 1, "{text}");
        // window: a zero window stops at the first chunk
        let p2 = d.path().join("host-y-1.log");
        let raw = RawLog::bounded(p2.clone(), Duration::ZERO, Duration::ZERO, 1 << 20);
        raw.push("child->relay", b"a");
        raw.push("child->relay", b"b");
        drop(raw);
        let t2 = std::fs::read_to_string(&p2).unwrap();
        assert_eq!(t2.matches("child->relay").count(), 0, "{t2}");
        assert_eq!(t2.matches("RAW-STREAM stopped").count(), 1, "{t2}");
    }

    #[test]
    fn a_full_host_log_rotates_to_dot_one_and_keeps_two_files() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("host-x-1.log");
        std::fs::write(&path, vec![b'a'; 100]).unwrap();
        rotate_if_full(&path, 100);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(d.path().join("host-x-1.log.1"))
                .unwrap()
                .len(),
            100
        );
        // a second rotation replaces the .1: two files at most
        std::fs::write(&path, vec![b'b'; 100]).unwrap();
        rotate_if_full(&path, 100);
        assert_eq!(
            std::fs::read(d.path().join("host-x-1.log.1")).unwrap()[0],
            b'b'
        );
        // control: a file under the cap is left alone
        std::fs::write(&path, b"short").unwrap();
        rotate_if_full(&path, 100);
        assert!(path.exists());
    }

    #[test]
    fn old_host_logs_are_pruned_and_nothing_else_is() {
        let d = tempfile::tempdir().unwrap();
        let w = |n: &str| std::fs::write(d.path().join(n), "x").unwrap();
        for n in [
            "host-fuel-100.log",
            "host-fuel-100.log.1",
            "host-ml-convertant-7.log",
            "host-fuel-abc.log",
            "host-.log",
            "restart.log",
            "launcher-store.json",
            "fuel.json",
        ] {
            w(n);
        }
        let later = std::time::SystemTime::now() + Duration::from_secs(25 * 3600);
        // one role only
        assert_eq!(
            prune_host_logs(d.path(), HOST_LOG_MAX_AGE, later, Some("fuel")),
            2
        );
        assert!(d.path().join("host-ml-convertant-7.log").exists());
        // every role; hyphenated roles parse; unrelated files survive
        assert_eq!(prune_host_logs(d.path(), HOST_LOG_MAX_AGE, later, None), 1);
        for n in [
            "host-fuel-abc.log",
            "host-.log",
            "restart.log",
            "launcher-store.json",
            "fuel.json",
        ] {
            assert!(d.path().join(n).exists(), "{n}");
        }
        // control: nothing is old at the real time
        w("host-fuel-200.log");
        assert_eq!(
            prune_host_logs(
                d.path(),
                HOST_LOG_MAX_AGE,
                std::time::SystemTime::now(),
                None
            ),
            0
        );
    }

    /// Issue #117: a gate for the startup window that opens when the child's output has
    /// carried `marker`, and the `on_raw_io` hook that watches for it. A window measured from
    /// spawn flaked whenever a loaded runner started `cmd.exe` after the window had closed; this
    /// one starts only once the child has really printed, so the tests wait on what they
    /// assert about, not on a guess at how long the child takes to start.
    fn window_gate_on(
        marker: &'static str,
    ) -> (
        Arc<std::sync::atomic::AtomicBool>,
        impl Fn(&str, &[u8]) + Send + Sync + 'static,
    ) {
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let gate_for_hook = Arc::clone(&gate);
        let hook = move |direction: &str, bytes: &[u8]| {
            if direction != "child->relay" {
                return;
            }
            if gate_for_hook.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let mut seen = seen.lock().unwrap();
            seen.extend_from_slice(bytes);
            if String::from_utf8_lossy(&seen).contains(marker) {
                gate_for_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        };
        (gate, hook)
    }

    // -- relay_chunk: the transparency property itself --------------------- //
    // PM finding, 2026-09-18: the host must be a transparent relay from
    // byte 0 - matching only OBSERVES, it never withholds, delays, or
    // rewrites a single byte, including mouse-mode and alt-screen
    // sequences (not just plain text).

    #[test]
    fn drain_tail_relays_what_the_reader_sent_after_the_loop_stopped() {
        // Chunks already queued, then the reader ends (its sender is dropped).
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        tx.send(b"one ".to_vec()).unwrap();
        tx.send(b"two ".to_vec()).unwrap();
        tx.send(b"FINAL".to_vec()).unwrap();
        drop(tx);
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        let n = drain_tail(
            &rx,
            &mut out,
            &mut parser,
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!((n, out.as_slice()), (3, &b"one two FINAL"[..]));

        // A chunk that arrives a little later is relayed too (the reader is still going).
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            tx.send(b"LATE".to_vec()).unwrap();
        });
        let mut out = Vec::new();
        let n = drain_tail(
            &rx,
            &mut out,
            &mut parser,
            Instant::now() + Duration::from_secs(5),
        );
        sender.join().unwrap();
        assert_eq!((n, out.as_slice()), (1, &b"LATE"[..]));

        // A reader that never ends does not hang the teardown: the deadline bounds it.
        let (_tx_alive, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let started = Instant::now();
        let n = drain_tail(
            &rx,
            &mut Vec::new(),
            &mut parser,
            Instant::now() + Duration::from_millis(80),
        );
        assert_eq!(n, 0);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn relay_chunk_forwards_plain_text_byte_for_byte() {
        let chunk = b"hello, world\r\n";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk);
    }

    #[test]
    fn relay_chunk_forwards_mouse_mode_sequences_byte_for_byte() {
        // \x1b[?1000h / \x1b[?1000l - mouse tracking on/off.
        let chunk = b"\x1b[?1000hclick here\x1b[?1000l";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk);
    }

    #[test]
    fn relay_chunk_forwards_alt_screen_sequences_byte_for_byte() {
        // \x1b[?1049h / \x1b[?1049l - alternate screen buffer on/off.
        let chunk = b"\x1b[?1049hfull screen app\x1b[?1049l";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk);
    }

    #[test]
    fn relay_chunk_forwards_a_long_mixed_stream_byte_for_byte() {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"\x1b[?9001h\x1b[?1004h"); // ConPTY negotiation
        chunk.extend_from_slice(b"\x1b[6n"); // CPR - never answered by this fn
        chunk.extend_from_slice(b"\x1b[?1049h"); // alt screen on
        chunk.extend_from_slice(b"some plain text\r\n");
        chunk.extend_from_slice(b"\x1b[?1000h\x1b[?1002h\x1b[?1015h\x1b[?1006h"); // mouse modes
        chunk.extend_from_slice(&[0u8, 1, 2, 255, 254]); // arbitrary binary bytes
        chunk.extend_from_slice(b"\x1b[?1049l"); // alt screen off
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        relay_chunk(&chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk, "every byte must survive the relay unchanged");
    }

    #[test]
    fn relay_chunk_never_answers_a_cpr_itself() {
        // ⚠️ PM finding, 2026-09-18: in production, WT (the real terminal)
        // answers claude's cursor-position-reports. The host must NOT -
        // that was only ever needed in the scratch probe, where nothing
        // real was on the other end.
        let chunk = b"\x1b[6n";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(24).unwrap(),
            std::num::NonZeroU16::new(80).unwrap(),
            0,
        );
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(
            out, chunk,
            "relay_chunk must forward exactly the CPR bytes and nothing else - \
             answering it is the real terminal's job, never this function's"
        );
    }

    // -- check_and_inject: exact-match handler firing ------------------------ //

    fn handler_json(id: &str, anchor: &str, action: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "match": {{"text_anchors": ["{anchor}"], "fields": {{}}}},
                "action": "{action}",
                "scope": {{"roles": ["overmind"]}},
                "provenance": {{"approved_by": "CireSnave", "approvals": [{{"said_on": "2026-09-18", "quote": "q", "approving": "a test"}}]}}
            }}"#
        )
    }

    #[test]
    fn check_and_inject_writes_the_action_on_an_exact_match() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\r")).unwrap();
        let mut input = Vec::new();
        let mut matched = HashSet::new();
        let mut answered = Vec::new();
        check_and_inject(
            &[handler],
            "overmind",
            lane_state::timestamp::Timestamp::now(),
            "please CONFIRM DIALOG now",
            &mut matched,
            &mut input,
            &mut |a| answered.push(a.handler_id),
        )
        .unwrap();
        assert_eq!(input, b"y\r");
        assert_eq!(answered, vec!["h1".to_string()]);
    }

    #[test]
    fn check_and_inject_sends_down_then_enter_as_the_ansi_bytes() {
        // The folder-trust dialog: only Down then Enter answers Yes.
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("trust", "CONFIRM DIALOG", "<Down><Enter>"))
                .unwrap();
        let mut input = Vec::new();
        let mut matched = HashSet::new();
        let mut answered = Vec::new();
        check_and_inject(
            &[handler],
            "overmind",
            lane_state::timestamp::Timestamp::now(),
            "please CONFIRM DIALOG now",
            &mut matched,
            &mut input,
            &mut |a| answered.push(a.injected_action),
        )
        .unwrap();
        assert_eq!(input, b"\x1b[B\r");
        assert_eq!(
            answered,
            vec!["<Down><Enter>".to_string()],
            "the ledger keeps the declared text"
        );
    }

    #[test]
    fn check_and_inject_never_fires_the_same_handler_twice() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\r")).unwrap();
        let mut input = Vec::new();
        let mut matched = HashSet::new();
        let mut answered = Vec::new();
        for _ in 0..3 {
            check_and_inject(
                std::slice::from_ref(&handler),
                "overmind",
                lane_state::timestamp::Timestamp::now(),
                "please CONFIRM DIALOG now",
                &mut matched,
                &mut input,
                &mut |a| answered.push(a.handler_id),
            )
            .unwrap();
        }
        assert_eq!(input, b"y\r", "the action must be written exactly once");
        assert_eq!(answered.len(), 1);
    }

    #[test]
    fn check_and_inject_does_nothing_when_no_handler_matches() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\r")).unwrap();
        let mut input = Vec::new();
        let mut matched = HashSet::new();
        let mut answered = Vec::new();
        check_and_inject(
            &[handler],
            "overmind",
            lane_state::timestamp::Timestamp::now(),
            "totally unrelated screen text",
            &mut matched,
            &mut input,
            &mut |a| answered.push(a.handler_id),
        )
        .unwrap();
        assert!(input.is_empty());
        assert!(answered.is_empty());
    }

    // -- run_with_handlers: real ConPTY, real child, full pipeline --------- //
    // ⚠️ THE REAL-PROCESS TEST: proves the actual mechanism works, not just
    // its pieces in isolation - the same discipline `facts.rs`/
    // `lane_state_writer.rs` already apply to anything touching a real OS
    // process. No REAL terminal exists on the outer side of a test, so
    // (matching what a real terminal like wt.exe would do) the fake outer
    // input answers the child's own CPR request itself.

    // -- RawConsoleGuard: mode set/restore, never a real-console property --- //
    // ⚠️ PM's own words: "A unit test can't reproduce a real console." This
    // proves only what CAN be proven without one: `enable()`/`Drop` never
    // panic or hang, and only ever touch a handle whose `GetConsoleMode`
    // succeeds - safe to call regardless of whether the TEST process's own
    // stdin/stdout happen to be a real console or (as under `cargo test`,
    // usually) redirected.

    #[test]
    #[cfg(windows)]
    fn raw_console_guard_enable_and_drop_never_panics() {
        let guard = RawConsoleGuard::enable();
        drop(guard);
    }

    // -- the unix tty guard and size, against a REAL pty -------------------- //

    #[cfg(unix)]
    fn open_pty_slave(
        rows: u16,
        cols: u16,
    ) -> (Box<dyn portable_pty::MasterPty + Send>, std::fs::File) {
        use portable_pty::{native_pty_system, PtySize};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pty");
        let name = pair.master.tty_name().expect("a pty slave has a name");
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .expect("open the slave");
        (pair.master, slave)
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_guard_makes_a_real_tty_raw_and_restores_it() {
        use rustix::termios::{tcgetattr, LocalModes};
        use std::os::fd::AsFd;
        let (_master, slave) = open_pty_slave(30, 90);
        let before = tcgetattr(&slave).unwrap();
        assert!(
            before.local_modes.contains(LocalModes::ICANON),
            "control: a fresh pty starts canonical"
        );
        {
            let guard = RawConsoleGuard::enable_on(slave.as_fd());
            assert!(guard.is_active());
            let during = tcgetattr(&slave).unwrap();
            assert!(!during.local_modes.contains(LocalModes::ICANON));
            assert!(!during.local_modes.contains(LocalModes::ECHO));
        }
        let after = tcgetattr(&slave).unwrap();
        assert_eq!(after.local_modes, before.local_modes);
        assert_eq!(after.input_modes, before.input_modes);
        assert_eq!(after.output_modes, before.output_modes);
    }

    /// Only a real tty is touched: a regular file (a redirected stdin) is left alone.
    #[cfg(unix)]
    #[test]
    fn the_unix_guard_leaves_a_non_tty_alone() {
        use std::os::fd::AsFd;
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("not-a-tty")).unwrap();
        assert!(!RawConsoleGuard::enable_on(f.as_fd()).is_active());
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_console_size_is_the_ttys_and_none_for_a_non_tty() {
        use std::os::fd::AsFd;
        let (_master, slave) = open_pty_slave(30, 90);
        assert_eq!(tty_size_of(slave.as_fd()), Some((90, 30)));
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("not-a-tty")).unwrap();
        assert_eq!(tty_size_of(f.as_fd()), None);
    }

    // -- output-driven CPR round trip: no pre-seeded knowledge -------------- //
    // ⚠️ PM finding, 2026-09-19 (real-restart-2, host-restarttest-34276.log):
    // a real restart never got past a blank screen - the host log showed
    // exactly one (blank) SCREEN CHANGED entry and nothing further. Every
    // OTHER real-ConPTY test above PRE-SEEDS the CPR answer into the fake
    // outer input, which never actually proves the host's own stdin-
    // forwarding thread is what delivers it - a pre-seeded Cursor answers
    // immediately regardless of whether that thread runs at all. THIS test
    // has no such foreknowledge: a fake "terminal" thread only reacts to
    // what it OBSERVES on the host's real stdout (exactly what WT itself
    // does for any process it hosts), and its reply only reaches the child
    // if the host's own stdin-forwarding thread is actually running and
    // correctly wired.

    /// A `Write` that hands each chunk to a channel - stands in for the
    /// host's own real stdout, observed by the fake terminal thread below.
    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .send(buf.to_vec())
                .map_err(|_| std::io::Error::other("fake terminal gone"))?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A `Read` fed by a channel - stands in for the host's own real
    /// stdin, written to only by the fake terminal thread below, only once
    /// it has actually observed a CPR request.
    struct ChanReader {
        rx: std::sync::mpsc::Receiver<Vec<u8>>,
        pending: Vec<u8>,
        pos: usize,
    }
    impl Read for ChanReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.pending.len() {
                match self.rx.recv() {
                    Ok(chunk) => {
                        self.pending = chunk;
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0),
                }
            }
            let n = (self.pending.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn run_with_handlers_completes_the_cpr_round_trip_driven_only_by_observing_output() {
        // host's real stdout -> what the fake terminal "sees".
        let (host_out_tx, host_out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // the fake terminal's reply -> the host's real stdin.
        let (term_reply_tx, term_reply_rx) = std::sync::mpsc::channel::<Vec<u8>>();

        let outer_output = ChanWriter(host_out_tx);
        let outer_input = ChanReader {
            rx: term_reply_rx,
            pending: Vec::new(),
            pos: 0,
        };

        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_thread = Arc::clone(&captured);

        // The fake terminal: reacts ONLY to what it observes, exactly like
        // a real terminal - no pre-seeded knowledge that a CPR is coming.
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut answered = false;
            while let Ok(chunk) = host_out_rx.recv() {
                captured_for_thread
                    .lock()
                    .unwrap()
                    .extend_from_slice(&chunk);
                seen.extend_from_slice(&chunk);
                if !answered && seen.windows(4).any(|w| w == b"\x1b[6n") {
                    let _ = term_reply_tx.send(b"\x1b[1;1R".to_vec());
                    answered = true;
                }
            }
        });

        let child_argv = platform_child(vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765".to_string(),
        ], "stty raw -echo; printf '\\033[6n'; dd bs=1 count=6 >/dev/null 2>&1; echo PROBE_MARKER_98765");
        let outcome = run_with_handlers(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            outer_output,
            |_| {},
            |_| {},
        )
        .expect("run_with_handlers failed");

        // Give the fake-terminal thread a moment to drain the child's
        // final chunk(s) after the process itself has already exited.
        std::thread::sleep(Duration::from_millis(200));
        let text = String::from_utf8_lossy(&captured.lock().unwrap()).to_string();
        assert!(
            text.contains("PROBE_MARKER_98765"),
            "the real banner must reach the fake terminal purely via an \
             output-driven CPR response, with no pre-seeded answer - got {text:?}"
        );
        assert!(outcome.child_exit_code.is_some());
    }

    #[test]
    fn run_with_handlers_hosts_a_real_child_and_relays_its_real_output() {
        // The outer "terminal" (fake, since this test has no real one):
        // answers the child's CPR the moment it would ask, exactly like a
        // real terminal does - this is content the FAKE TERMINAL supplies,
        // never something the host itself generates (see the transparency
        // tests above).
        let outer_input = Cursor::new(b"\x1b[1;1R".to_vec());
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765".to_string(),
            ],
            "echo PROBE_MARKER_98765",
        );
        let outcome = run_with_handlers(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            SharedVecWriter(Arc::clone(&outer_output)),
            |_| {},
            |_| {},
        )
        .expect("run_with_handlers failed");

        let captured = outer_output.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&captured);
        assert!(
            text.contains("PROBE_MARKER_98765"),
            "real child output must reach the outer side - got {text:?}"
        );
        assert!(outcome.child_exit_code.is_some());
    }

    /// The host's last output is not lost to the exit race: whatever the child
    /// printed last (here with no trailing newline, which a ConPTY may only
    /// flush when it is closed) has reached the outer side by the time
    /// `run_with_handlers` returns, with no sleep to cover for it.
    #[test]
    fn the_childs_final_output_reaches_the_outer_side_before_the_host_returns() {
        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for run in 0..5 {
            let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
            let child_argv = platform_child(
                vec![
                    "cmd.exe".to_string(),
                    "/c".to_string(),
                    "echo FIRST_LINE & <nul set /p =FINAL_TAIL_MARK_4242".to_string(),
                ],
                "echo FIRST_LINE; printf FINAL_TAIL_MARK_4242",
            );
            let outcome = run_with_handlers(
                "overmind",
                &child_argv,
                Vec::new(),
                80,
                25,
                Cursor::new(b"[1;1R".to_vec()),
                SharedVecWriter(Arc::clone(&outer_output)),
                |_| {},
                |_| {},
            )
            .expect("run_with_handlers failed");
            let text = String::from_utf8_lossy(&outer_output.lock().unwrap()).to_string();
            assert!(text.contains("FIRST_LINE"), "run {run}: {text:?}");
            assert!(
                text.contains("FINAL_TAIL_MARK_4242"),
                "run {run}: the child's last output was dropped: {text:?}"
            );
            assert!(outcome.child_exit_code.is_some());
        }
    }

    /// The PTY reader used to be abandoned in `ReadFile`, and the resize
    /// thread (which owns the pty master) never ended at all. Read back,
    /// per run, that no helper thread this run started is still alive.
    #[test]
    fn no_helper_thread_outlives_a_host_run() {
        for run in 0..3 {
            let child_argv = platform_child(
                vec![
                    "cmd.exe".to_string(),
                    "/c".to_string(),
                    "echo LEAK_PROBE".to_string(),
                ],
                "echo LEAK_PROBE",
            );
            let outcome = run_with_handlers(
                "overmind",
                &child_argv,
                Vec::new(),
                80,
                25,
                Cursor::new(b"[1;1R".to_vec()),
                std::io::sink(),
                |_| {},
                |_| {},
            )
            .expect("run_with_handlers failed");
            assert!(
                outcome.abandoned_threads.is_empty(),
                "run {run}: helper threads still alive after return: {:?} (reader ended {:?} after the child exited)",
                outcome.abandoned_threads,
                outcome.reader_end_after_exit
            );
        }
    }

    /// An outer input that reaches end-of-file shortly AFTER the child has
    /// exited is not a leak: the host waits a bounded time for it. Without
    /// that wait the teardown samples the thread's flag while it is still
    /// running and reports `outer-input` (seen on ubuntu CI, run 0).
    #[test]
    fn an_outer_input_that_ends_just_after_the_child_is_not_abandoned() {
        struct LateEof {
            sent_cpr: bool,
            child_output_seen: Arc<std::sync::atomic::AtomicBool>,
        }
        struct FlagOnWrite(Arc<std::sync::atomic::AtomicBool>);
        impl Write for FlagOnWrite {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Read for LateEof {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.sent_cpr {
                    self.sent_cpr = true;
                    let cpr = b"[1;1R";
                    buf[..cpr.len()].copy_from_slice(cpr);
                    return Ok(cpr.len());
                }
                // End-of-file comes 500 ms after the child's output reached the
                // outer side, so it always lands after the child has exited.
                while !self
                    .child_output_seen
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(500));
                Ok(0)
            }
        }
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo LEAK_PROBE".to_string(),
            ],
            "echo LEAK_PROBE",
        );
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let outcome = run_with_handlers(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            LateEof {
                sent_cpr: false,
                child_output_seen: Arc::clone(&seen),
            },
            FlagOnWrite(seen),
            |_| {},
            |_| {},
        )
        .expect("run_with_handlers failed");
        assert!(
            outcome.abandoned_threads.is_empty(),
            "helper threads still alive after return: {:?}",
            outcome.abandoned_threads
        );
    }

    /// An outer input that is still blocked cannot be cancelled through a
    /// generic `Read`; the host must say so rather than report a clean run.
    /// Positive control for `no_helper_thread_outlives_a_host_run`: the
    /// same run, with one thread deliberately kept alive.
    #[test]
    fn a_blocked_outer_input_is_reported_as_abandoned() {
        let (keep_open, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // Preloaded with the terminal's CPR reply (ConPTY holds the child's
        // output until it gets one), then blocks for good.
        let outer_input = ChanReader {
            rx,
            pending: b"[1;1R".to_vec(),
            pos: 0,
        };
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo LEAK_PROBE".to_string(),
            ],
            "echo LEAK_PROBE",
        );
        let outcome = run_with_handlers(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            std::io::sink(),
            |_| {},
            |_| {},
        )
        .expect("run_with_handlers failed");
        assert_eq!(outcome.abandoned_threads, vec!["outer-input"]);
        drop(keep_open);
    }

    #[test]
    fn run_with_handlers_spawns_the_child_in_this_processs_own_current_directory() {
        // ⚠️ THE ROOT CAUSE, PM finding 2026-09-19 (real-restart-04:39:56Z):
        // `portable_pty::CommandBuilder` with no explicit `.cwd()` falls
        // back to `USERPROFILE`, NOT this process's own cwd - `claude.exe`
        // was silently spawned in the WRONG directory, which is why
        // `find_claude_process_in`'s cwd match never found it. This proves
        // the fix directly: the real child's own reported cwd (via `cmd /c
        // cd`, which prints the process's actual working directory) must
        // equal THIS test process's own `current_dir()` - never
        // `USERPROFILE`, which would be a different, wrong answer whenever
        // the two differ (as they do for a normal `cargo test` run).
        let outer_input = Cursor::new(b"\x1b[1;1R".to_vec());
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let child_argv = platform_child(
            vec!["cmd.exe".to_string(), "/c".to_string(), "cd".to_string()],
            "pwd",
        );
        run_with_handlers(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            SharedVecWriter(Arc::clone(&outer_output)),
            |_| {},
            |_| {},
        )
        .expect("run_with_handlers failed");

        let captured = outer_output.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&captured);
        let expected = std::env::current_dir().unwrap();
        assert!(
            text.contains(expected.to_string_lossy().as_ref()),
            "the real child's own cwd must be this process's current_dir() \
             ({expected:?}) - got screen text {text:?}"
        );
    }

    #[test]
    fn run_with_handlers_matches_and_answers_a_real_dialog_from_a_real_child() {
        // A real child that prints something matching a test handler's
        // exact text, then BLOCKS on its own stdin (`set /p`) waiting for a
        // reply - proves the FULL loop (real ConPTY -> vt100 screen model ->
        // exact match -> real injected keystroke fed back into the real
        // child's real stdin) end to end, not simulated at any layer, and
        // deterministically (no timing-dependent write into the middle of
        // the fake outer input - the marker is the child's OWN first line
        // of output, not something a test thread has to race to send).
        // `/c` means the child exits on its own once the echo after the
        // reply completes, so the read loop terminates without needing an
        // external kill.
        let handler: HandlerSpec = serde_json::from_str(&handler_json(
            "echo-responder",
            "PROBE_MARKER_98765",
            "y\\r",
        ))
        .unwrap();

        // ConPTY asks the terminal for the cursor position and consumes the answer itself; a unix
        // pty never asks, so a pre-seeded reply would arrive at the child as typed input and land
        // in its `read` (CI read, 2026-10-14: `GOT:^[[1;1RINJECTED_REPLY`).
        let outer_input = Cursor::new(if cfg!(windows) {
            b"\x1b[1;1R".to_vec()
        } else {
            Vec::new()
        });
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        // ⚠️ `%REPLY%` expands at PARSE time, before `set /p` ever runs, so
        // it would stay the literal text `%REPLY%` regardless of what's
        // typed - `/v:on` (delayed expansion, from the start of THIS
        // invocation, unlike `setlocal enabledelayedexpansion` mid-line
        // which is still too late inside one `&`-chained command line) plus
        // `!REPLY!` is what makes this actually prove the injected text was
        // received.
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/v:on".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765 & set /p REPLY=waiting: & echo GOT:!REPLY!".to_string(),
            ],
            "echo PROBE_MARKER_98765; printf \"waiting: \"; read REPLY; echo GOT:$REPLY",
        );
        let answered = Arc::new(Mutex::new(Vec::new()));
        let answered_for_cb = Arc::clone(&answered);

        let outcome = run_with_handlers(
            "overmind",
            &child_argv,
            vec![handler],
            80,
            25,
            outer_input,
            SharedVecWriter(Arc::clone(&outer_output)),
            move |a| answered_for_cb.lock().unwrap().push(a.handler_id),
            |_| {},
        )
        .expect("run_with_handlers failed");

        assert_eq!(
            answered.lock().unwrap().as_slice(),
            &["echo-responder".to_string()],
            "the real child's real output must have driven a real match"
        );
        let captured = outer_output.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&captured);
        assert!(
            text.contains("GOT:y"),
            "the real child must have received the real injected keystrokes \
             back on its own stdin - got {text:?}"
        );
        assert!(outcome.child_exit_code.is_some());
    }

    #[test]
    fn a_footer_alone_is_told_from_a_dialog_with_a_body_and_from_a_blank_screen() {
        assert!(footer_only("\n\nEnter to confirm \u{b7} Esc to cancel\n"));
        assert!(footer_only("   Esc to cancel"));
        // Controls: a body above the footer, a blank screen, and one unrelated line.
        assert!(!footer_only(
            "Quick safety check: is this a project you trust?\nEnter to confirm \u{b7} Esc to cancel"
        ));
        assert!(!footer_only("\n\n"));
        assert!(!footer_only("PROBE_MARKER"));
    }

    /// 2026-11-22: a dialog whose body never reached the screen model sat unanswered for the whole
    /// window. The host now asks for a repaint (and says so in the host log) instead of waiting.
    #[test]
    fn a_degenerate_polled_console_size_is_never_forwarded_to_the_pty() {
        // The 2026-11-25 stalls: the console query said 2 rows x 46 columns (and about 10 wide).
        assert_eq!(forwardable_console_size((120, 30), (46, 2)), None);
        assert_eq!(forwardable_console_size((120, 30), (10, 30)), None);
        assert_eq!(forwardable_console_size((120, 30), (120, 9)), None);
        // Controls: a real change is forwarded, an unchanged size is not.
        assert_eq!(
            forwardable_console_size((120, 30), (100, 40)),
            Some((100, 40))
        );
        assert_eq!(forwardable_console_size((120, 30), (120, 30)), None);
        assert_eq!(forwardable_console_size((80, 25), (40, 10)), Some((40, 10)));
    }

    // Pip's review of #304, L1: the note for a degenerate size repeated every 500 ms (15 identical lines in ~7 s).
    #[test]
    fn a_degenerate_console_size_is_noted_once_not_every_poll() {
        let mut noted = None;
        let last = (120, 30);
        assert_eq!(
            poll_action(last, (46, 2), &mut noted),
            PollAction::NoteDegenerate
        );
        for _ in 0..15 {
            assert_eq!(
                poll_action(last, (46, 2), &mut noted),
                PollAction::Nothing,
                "same size: silent"
            );
        }
        // A different degenerate size is news.
        assert_eq!(
            poll_action(last, (10, 30), &mut noted),
            PollAction::NoteDegenerate
        );
        // Back to the pty's own size, then degenerate again: noted again.
        assert_eq!(poll_action(last, last, &mut noted), PollAction::Nothing);
        assert_eq!(
            poll_action(last, (10, 30), &mut noted),
            PollAction::NoteDegenerate
        );
    }

    #[test]
    fn a_real_console_resize_is_still_forwarded_and_clears_the_note() {
        let mut noted = Some((46, 2));
        assert_eq!(
            poll_action((120, 30), (100, 40), &mut noted),
            PollAction::Forward((100, 40))
        );
        assert_eq!(noted, None);
    }

    // L2: the pty was created at whatever the console query said, with no degenerate check.
    #[test]
    fn a_degenerate_console_size_at_creation_creates_the_pty_at_80x25_and_says_so() {
        let (size, why) = creation_size(Some((46, 2)));
        assert_eq!(size, (80, 25));
        assert!(
            why.contains("FALLBACK 80x25") && why.contains("46x2"),
            "{why}"
        );
        let (size, why) = creation_size(Some((10, 30)));
        assert_eq!(size, (80, 25));
        assert!(why.contains("10x30"), "{why}");
        // Controls: a real console is used as is, and a failed query still falls back.
        let (size, why) = creation_size(Some((120, 30)));
        assert_eq!(size, (120, 30));
        assert!(why.contains("the host's own console"), "{why}");
        let (size, why) = creation_size(None);
        assert_eq!(size, (80, 25));
        assert!(why.contains("the console size query failed"), "{why}");
    }

    // H1 (PM row 2026-11-26 ~05:25Z): after the dev-channels dialog answer the host asks the Synapse adapter to write
    // the lane's unacked mail again, by creating `redeliver-<role>.request` in the lane's Synapse home.
    #[test]
    fn a_dev_channels_answer_creates_the_redeliver_request_in_the_synapse_home() {
        let d = tempfile::tempdir().unwrap();
        let target = SynapseTarget {
            home: d.path().to_path_buf(),
            role: "launchtest".into(),
        };
        let note = request_redeliver_after_answer("synapse-dev-channel-any-lane", Some(&target));
        assert!(d.path().join("redeliver-launchtest.request").is_file());
        assert_eq!(
            std::fs::metadata(d.path().join("redeliver-launchtest.request"))
                .unwrap()
                .len(),
            0
        );
        assert!(note.unwrap().starts_with("REDELIVER requested"), "logged");
    }

    #[test]
    fn an_answer_that_is_not_a_dev_channels_dialog_requests_nothing() {
        let d = tempfile::tempdir().unwrap();
        let target = SynapseTarget {
            home: d.path().to_path_buf(),
            role: "launchtest".into(),
        };
        assert_eq!(
            request_redeliver_after_answer("trust-folder-prefix", Some(&target)),
            None
        );
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_unwritable_home_is_a_logged_note_never_a_failure() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        // A "home" below a regular file cannot hold anything.
        let target = SynapseTarget {
            home: file.join("home"),
            role: "launchtest".into(),
        };
        let note =
            request_redeliver_after_answer("synapse-dev-channel-any-lane", Some(&target)).unwrap();
        assert!(note.starts_with("REDELIVER not requested"), "{note}");
        let none = request_redeliver_after_answer("synapse-dev-channel-any-lane", None).unwrap();
        assert!(
            none.starts_with("REDELIVER not requested") && none.contains("no Synapse home"),
            "{none}"
        );
    }

    #[test]
    fn a_second_answer_finds_the_request_already_pending() {
        let d = tempfile::tempdir().unwrap();
        let target = SynapseTarget {
            home: d.path().to_path_buf(),
            role: "launchtest".into(),
        };
        let _ = request_redeliver_after_answer("synapse-dev-channel-any-lane", Some(&target));
        let note =
            request_redeliver_after_answer("claude-peers-dev-channels", Some(&target)).unwrap();
        assert!(note.contains("already pending"), "{note}");
    }

    /// The adapter's own helper (synapse `redeliver_request_path`, branch lane/h1-redeliver d7f0bdf3) keeps
    /// ASCII alphanumerics, `-` and `_` and turns everything else into `_`: the name must match exactly.
    #[test]
    fn the_request_file_name_sanitises_the_role_as_the_adapter_does() {
        let h = std::path::Path::new("H");
        assert_eq!(
            redeliver_request_path(h, "launchtest"),
            h.join("redeliver-launchtest.request")
        );
        assert_eq!(
            redeliver_request_path(h, "a/b c.d"),
            h.join("redeliver-a_b_c_d.request")
        );
        assert_eq!(
            redeliver_request_path(h, "agent-loop_2"),
            h.join("redeliver-agent-loop_2.request")
        );
    }

    #[test]
    fn the_target_is_the_home_and_role_the_mcp_config_gives_the_adapter() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("launchtest.mcp.json");
        std::fs::write(&cfg, r#"{"mcpServers":{"synapse":{"args":["--role","launchtest","--home","C:/h/launchtest"],"command":"x"}}}"#).unwrap();
        let argv = vec![
            "claude".to_string(),
            "--mcp-config".to_string(),
            cfg.display().to_string(),
        ];
        let t = synapse_target("fallback-role", &argv).unwrap();
        assert_eq!(
            (t.home, t.role.as_str()),
            (std::path::PathBuf::from("C:/h/launchtest"), "launchtest")
        );
        // No --mcp-config, or no readable file: no target (the host then logs why).
        assert!(synapse_target("r", &["claude".to_string()]).is_none());
        assert!(synapse_target(
            "r",
            &[
                "claude".to_string(),
                "--mcp-config".to_string(),
                "Z:/nope.json".to_string()
            ]
        )
        .is_none());
    }

    // N1: the REDRAW note said the pty "resized once" before the resize thread had acted.
    #[test]
    fn the_redraw_request_note_does_not_claim_the_resize_happened() {
        let text = redraw_request_note(2);
        assert!(text.contains("REDRAW requested"), "{text}");
        assert!(!text.contains("resized once"), "{text}");
        assert!(
            text.contains("PTY SIZE REDRAW"),
            "the outcome is the resize thread's line: {text}"
        );
    }

    #[test]
    fn a_dialog_footer_with_no_body_gets_repaint_requests_in_the_host_log() {
        let notes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let notes_for_cb = Arc::clone(&notes);
        struct Sink;
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo Enter to confirm . Esc to cancel & choice /t 7 /d y >nul".to_string(),
            ],
            "echo 'Enter to confirm . Esc to cancel'; sleep 7",
        );
        run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            // The reply to the ConPTY startup cursor query is only needed on Windows. A Unix pty ECHOES it onto the
            // screen as `^[[1;1R`; when that lands after the child's output the footer is no longer alone and no
            // repaint is asked for (CI main fe03f444 and #302 db374d1; reproduced with 12 parallel runs on 2 cores).
            Cursor::new(if cfg!(windows) {
                b"[1;1R".to_vec()
            } else {
                Vec::new()
            }),
            Sink,
            |_| {},
            |_| {},
            Duration::from_secs(6),
            |_, _| {},
            move |direction, bytes| {
                if direction == "host-note" {
                    notes_for_cb
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(bytes).into_owned());
                }
            },
            None,
            None,
        )
        .expect("run failed");
        let notes = notes.lock().unwrap();
        assert!(
            !notes.is_empty() && notes[0].starts_with("REPAINT 1/"),
            "a footer alone for {REPAINT_AFTER:?} must ask for a repaint, notes: {notes:?}"
        );
    }

    // -- on_unhandled: §12.6 unhandled-prompt capture --------------------- //
    // A tiny `startup_window` (via `run_with_handlers_and_window`, not the
    // real 60s `STARTUP_WINDOW`) is what makes these fast without waiting
    // it out for real.

    #[test]
    fn on_unhandled_fires_once_with_the_screen_text_when_nothing_ever_matched() {
        let outer_input = Cursor::new(b"\x1b[1;1R".to_vec());
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        // No handler is loaded (`Vec::new()` below), so nothing will ever
        // inject a reply - `choice` is used instead of `set /p` so the
        // child still terminates ON ITS OWN (auto-selects after 2s)
        // rather than blocking forever on input that will never arrive.
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765 & choice /t 2 /d y >nul".to_string(),
            ],
            "echo PROBE_MARKER_98765; sleep 2",
        );
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_for_cb = Arc::clone(&captured);

        let (gate, on_raw_io) = window_gate_on("PROBE_MARKER_98765");
        let outcome = run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            Vec::new(), // no handlers loaded - nothing can ever match
            80,
            25,
            outer_input,
            SharedVecWriter(outer_output),
            |_| {},
            move |text| captured_for_cb.lock().unwrap().push(text),
            // Long enough for the real child to actually spawn and print
            // its marker before the window ends, short enough (and well
            // before `choice`'s own 2s auto-continue) to stay a fast unit
            // test rather than waiting out the real 60s STARTUP_WINDOW.
            Duration::from_millis(800),
            |_, _| {},
            on_raw_io,
            None,
            Some(gate),
        )
        .expect("run_with_handlers_and_window_watched failed");

        let captures = captured.lock().unwrap();
        assert_eq!(
            captures.len(),
            1,
            "must capture exactly once, not per poll - got {captures:?}"
        );
        assert!(
            captures[0].contains("PROBE_MARKER_98765"),
            "the captured text must be real screen content - got {:?}",
            captures[0]
        );
        assert!(outcome.child_exit_code.is_none() || outcome.child_exit_code.is_some());
    }

    /// Issue #117: the window is a wall-clock budget from spawn, so a child that
    /// is slow to print (a loaded runner, a cold `cmd.exe`) is captured while
    /// the screen is still blank. The child here prints its marker 2 s after
    /// spawn, deterministically later than the 800 ms window.
    #[test]
    fn on_unhandled_captures_a_child_whose_first_output_is_late() {
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "choice /t 2 /d y >nul & echo PROBE_MARKER_98765 & choice /t 3 /d y >nul"
                    .to_string(),
            ],
            "sleep 2; echo PROBE_MARKER_98765; sleep 3",
        );
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_for_cb = Arc::clone(&captured);

        let (gate, on_raw_io) = window_gate_on("PROBE_MARKER_98765");
        run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            Cursor::new(b"[1;1R".to_vec()),
            std::io::sink(),
            |_| {},
            move |text| captured_for_cb.lock().unwrap().push(text),
            Duration::from_millis(800),
            |_, _| {},
            on_raw_io,
            None,
            Some(gate),
        )
        .expect("run_with_handlers_and_window_watched failed");

        let captures = captured.lock().unwrap();
        assert_eq!(
            captures.len(),
            1,
            "must capture exactly once - got {captures:?}"
        );
        assert!(
            captures[0].contains("PROBE_MARKER_98765"),
            "the capture must hold the child's real output, not a blank screen - got {:?}",
            captures[0]
        );
    }

    #[test]
    fn on_unhandled_never_fires_when_a_handler_matched_during_the_window() {
        let handler: HandlerSpec = serde_json::from_str(&handler_json(
            "echo-responder",
            "PROBE_MARKER_98765",
            "y\\r",
        ))
        .unwrap();

        let outer_input = Cursor::new(b"\x1b[1;1R".to_vec());
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765 & set /p REPLY=waiting: & echo done".to_string(),
            ],
            "echo PROBE_MARKER_98765; read REPLY; echo done",
        );
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_for_cb = Arc::clone(&captured);

        let (gate, on_raw_io) = window_gate_on("PROBE_MARKER_98765");
        run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            vec![handler],
            80,
            25,
            outer_input,
            SharedVecWriter(outer_output),
            |_| {},
            move |text| captured_for_cb.lock().unwrap().push(text),
            Duration::from_millis(1500),
            |_, _| {},
            on_raw_io,
            None,
            Some(gate),
        )
        .expect("run_with_handlers_and_window_watched failed");

        assert!(
            captured.lock().unwrap().is_empty(),
            "a real match must suppress the unhandled-prompt capture entirely"
        );
    }

    /// Issue #96 (a): `on_unhandled` sends the PM ask over the claude-peers
    /// broker, and a stalled broker must not freeze the terminal. The
    /// callback here blocks until released (or 90s; the watcher's own 60s deadline
    /// always decides first, so a slow runner cannot make the callback return on its own); the child prints a
    /// second marker ~1s after the window ends. That marker must reach the
    /// outer terminal WHILE the callback is still blocked - inline on the
    /// relay thread, it can only arrive after the 10s timeout.
    #[test]
    fn a_blocked_on_unhandled_never_stalls_the_relay() {
        let outer_input = Cursor::new(b"\x1b[1;1R".to_vec());
        let outer_output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765 & choice /t 2 /d y >nul & echo AFTER_MARKER_24680"
                    .to_string(),
            ],
            "echo PROBE_MARKER_98765; sleep 2; echo AFTER_MARKER_24680",
        );
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let callback_returned = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let returned_for_cb = Arc::clone(&callback_returned);

        // Watches the outer terminal for the second marker, records whether
        // the callback was still blocked at that moment, then releases it.
        let output_for_watch = Arc::clone(&outer_output);
        let returned_for_watch = Arc::clone(&callback_returned);
        let watcher = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut relayed_while_blocked = None;
            while Instant::now() < deadline {
                let seen = String::from_utf8_lossy(&output_for_watch.lock().unwrap())
                    .contains("AFTER_MARKER_24680");
                if seen {
                    relayed_while_blocked =
                        Some(!returned_for_watch.load(std::sync::atomic::Ordering::SeqCst));
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = release_tx.send(());
            relayed_while_blocked
        });

        let (gate, on_raw_io) = window_gate_on("PROBE_MARKER_98765");
        run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            SharedVecWriter(Arc::clone(&outer_output)),
            |_| {},
            move |_text| {
                let _ = release_rx.recv_timeout(Duration::from_secs(90));
                returned_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            Duration::from_millis(800),
            |_, _| {},
            on_raw_io,
            None,
            Some(gate),
        )
        .expect("run_with_handlers_and_window_watched failed");

        assert_eq!(
            watcher.join().unwrap(),
            Some(true),
            "the child's later output must reach the terminal while on_unhandled is still blocked"
        );
        assert!(
            callback_returned.load(std::sync::atomic::Ordering::SeqCst),
            "the host must still wait for the ask to finish before returning"
        );
    }

    /// Issue #96 (a), the other half: with the ask off the relay thread, a
    /// child that exits while the ask is still in flight must not let the
    /// host return (and the process exit) mid-ask. The callback here
    /// outlives the child by ~2s.
    #[test]
    fn the_host_waits_for_an_in_flight_on_unhandled_before_returning() {
        let child_argv = platform_child(
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo PROBE_MARKER_98765 & choice /t 2 /d y >nul".to_string(),
            ],
            "echo PROBE_MARKER_98765; sleep 2",
        );
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished_for_cb = Arc::clone(&finished);

        let (gate, on_raw_io) = window_gate_on("PROBE_MARKER_98765");
        run_with_handlers_and_window_watched(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            Cursor::new(b"\x1b[1;1R".to_vec()),
            std::io::sink(),
            |_| {},
            move |_text| {
                std::thread::sleep(Duration::from_secs(4));
                finished_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            Duration::from_millis(300),
            |_, _| {},
            on_raw_io,
            None,
            Some(gate),
        )
        .expect("run_with_handlers_and_window_watched failed");

        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "run must not return while on_unhandled is still running"
        );
    }

    // -- folder-trust prefix approvals at the host ------------------------------------------------------

    const PREFIX_JSON: &str = include_str!("../approval-example/trust-folder-prefix.json");

    fn trust_dialog(path: &str) -> String {
        format!(
            "\n Accessing workspace:\n\n {path}\n\n Quick safety check: Is this a project you created or one you trust?\n\n \
             ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n"
        )
    }

    #[test]
    fn a_folder_trust_prefix_answer_sends_down_enter_and_reports_the_resolved_folder() {
        let fs = crate::folder_trust::staged::StagedFs::with(&[r"C:\Projects\newlane"])
            .alias(r"C:\Projects\evil", r"D:\x");
        let handler: HandlerSpec = serde_json::from_str(PREFIX_JSON).unwrap();
        let run = |path: &str| {
            let mut input = Vec::new();
            let mut answered = Vec::new();
            check_and_inject_with(
                &fs,
                std::slice::from_ref(&handler),
                "newlane",
                lane_state::timestamp::Timestamp::now(),
                &trust_dialog(path),
                &mut HashSet::new(),
                &mut input,
                &mut |a| answered.push((a.handler_id, a.resolved_folder)),
            )
            .unwrap();
            (input, answered)
        };
        let (input, answered) = run(r"C:\Projects\newlane");
        assert_eq!(input, handlers::action_bytes("<Down><Enter>").unwrap());
        assert_eq!(
            answered,
            vec![(
                "trust-folder-prefix".to_string(),
                Some(r"C:\Projects\newlane".to_string())
            )]
        );
        // The junction out of the prefix: nothing is sent, nothing is reported.
        let (input, answered) = run(r"C:\Projects\evil");
        assert!(
            input.is_empty() && answered.is_empty(),
            "{input:?} {answered:?}"
        );
    }

    #[test]
    fn a_folder_trust_answer_is_logged_for_the_pm_and_announced_as_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let host_log = dir.path().join("host.log");
        record_folder_trust_answer(
            dir.path(),
            "newlane",
            "trust-folder-prefix",
            r"C:\Projects\newlane",
            "pm",
            notify::Transport::Synapse,
            &host_log,
        );
        let log = std::fs::read_to_string(dir.path().join("folder-trust-answers.log")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(line["lane"], "newlane");
        assert_eq!(line["handler"], "trust-folder-prefix");
        assert_eq!(line["folder"], r"C:\Projects\newlane");
        assert!(line["time"].as_str().is_some_and(|t| t.contains('T')));
        // The notice is queued on its own thread: wait for it.
        let outbox = notify::outbox_dir(dir.path());
        let deadline = Instant::now() + Duration::from_secs(5);
        let queued = loop {
            // `notify::queue` writes `<stamp>.tmp` and renames it to `<stamp>.json`: take only a finished
            // file, never the temporary one that is gone by the time it is read (the flake: 2 of 25
            // full-suite runs failed at the `read_to_string` on a `.tmp` that had just been renamed).
            let files: Vec<_> = std::fs::read_dir(&outbox)
                .map(|d| {
                    d.filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|x| x == "json"))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(f) = files.into_iter().next() {
                break std::fs::read_to_string(f).unwrap();
            }
            assert!(Instant::now() < deadline, "the notice was never queued");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            queued.contains("folder-trust-auto-answer") && queued.contains("newlane"),
            "{queued}"
        );
        assert!(
            queued.contains(r"C:\\Projects\\newlane") || queued.contains(r"C:\Projects\newlane"),
            "{queued}"
        );
        assert!(!queued.contains('\n') || queued.matches("[NOTICE]").count() == 1);
    }
}
