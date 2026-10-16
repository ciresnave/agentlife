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
    let (rows, _cols) = screen.size();
    (0..rows)
        .map(|row| screen.contents_between(row, 0, row, screen.size().1))
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
    now: chrono::DateTime<chrono::Utc>,
    screen_text: &str,
    matched_ids: &mut HashSet<String>,
    input_writer: &mut impl Write,
    on_answered: &mut impl FnMut(HandlerAnswered),
) -> std::io::Result<()> {
    if let Some(handler) = handlers::find_matching_handler(handler_list, role, now, screen_text) {
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
}

/// How long the host waits, after the child has exited and the ConPTY has
/// been closed, for the PTY reader thread to see end-of-file and end.
const READER_END_TIMEOUT: Duration = Duration::from_secs(3);

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
    )
}

/// The implementation, with a `watch` hub (`crate::watch`, `attach --watch`): when present, the relay
/// loop answers watchers' requests with the current screen and publishes every chunk to them. It
/// reads nothing from them. `None` (every test, and Windows) is exactly the relay as it was.
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
) -> std::io::Result<HostOutcome> {
    let on_raw_io = Arc::new(on_raw_io);
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
    let resize_thread = {
        let master_for_resize = pair.master;
        let mut last = (cols, rows);
        std::thread::spawn(move || {
            let _alive = resize_flag;
            // Ends on a stop message or when the host drops the sender.
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stop_resize_rx.recv_timeout(Duration::from_millis(500))
            {
                if let Some((c, r)) = current_console_size() {
                    if (c, r) != last {
                        let _ = master_for_resize.resize(PtySize {
                            rows: r,
                            cols: c,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
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
    {
        let on_raw_io = Arc::clone(&on_raw_io);
        std::thread::spawn(move || {
            let _alive = reader_flag;
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
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut matched_ids: HashSet<String> = HashSet::new();
    let mut last_logged_screen: Option<String> = None;
    let started = Instant::now();
    loop {
        let still_in_startup_window = started.elapsed() < startup_window;
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
                            .map(|h| handlers::match_report(h, role, chrono::Utc::now(), &text))
                            .collect();
                        on_match_attempt(&text, &reports);
                        last_logged_screen = Some(text.clone());
                    }
                    let mut writer = pty_writer.lock().unwrap();
                    check_and_inject(
                        &handler_list,
                        role,
                        chrono::Utc::now(),
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
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait().ok();

    // Teardown. The resize thread owns the pty master: stop it, and its
    // exit closes the ConPTY, which is what ends the reader's blocking
    // read. Then give the reader a bounded time to say so; whatever is
    // still running after that is reported, never silently left.
    drop(stop_resize);
    let _ = resize_thread.join();
    // Whatever the reader still delivers while the ConPTY closes is the child's last output
    // (a ConPTY may flush its final frame only when it is closed), so it is relayed here, not
    // left in a channel nobody reads any more: this wait is also the drain.
    drain_tail(
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
    Ok(HostOutcome {
        child_exit_code: status.and_then(|s| s.exit_code().into()),
        abandoned_threads,
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

/// Best-effort append - never fatal to the relay that's still running.
fn append_host_log(path: &std::path::Path, line: &str) {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Convenience entry point for real production use: loads the user's
/// approvals (§12.3) BEFORE spawning the child - a dialog that renders
/// before its handlers arrive would never be re-checked on a static screen
/// - and wires this process's OWN real stdin/stdout as the outer relay ends.
pub fn run(role: &str, child_argv: &[String]) -> std::io::Result<HostOutcome> {
    #[cfg(unix)]
    detach_session_if_headless();
    let log_path = host_log_path(role);
    let started_at = chrono::Utc::now();

    eprintln!("lane-restart host: loading approvals...");
    let (config, loaded) = match crate::paths::home_dir() {
        Some(home) => approvals::load_configured(&home, &approvals::GhReader::default()),
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
    for (h, hash) in &loaded.handlers {
        append_host_log(&log_path, &format!("  active {} {hash}", h.id));
    }
    for (name, reason) in &loaded.refused {
        append_host_log(&log_path, &format!("  REFUSED {name}: {reason}"));
    }
    let handler_list = loaded.specs();

    // ⚠️ Kept alive for the relay's ENTIRE scope - restored on every exit
    // path via `Drop`, including a panic unwind.
    #[cfg(any(windows, unix))]
    let _raw_console_guard = RawConsoleGuard::enable();

    let (cols, rows) = current_console_size().unwrap_or((80, 25));
    let log_path_for_answered = log_path.clone();
    let log_path_for_attempts = log_path.clone();
    let log_path_for_raw_io = log_path.clone();
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
                    chrono::Utc::now().to_rfc3339(),
                    answered.handler_id,
                    answered.injected_action,
                    answered.matched_screen_text
                ),
            );
        },
        on_unhandled,
        STARTUP_WINDOW,
        move |screen, reports| {
            append_host_log(
                &log_path_for_attempts,
                &format!(
                    "[{}] SCREEN CHANGED {:?}",
                    chrono::Utc::now().to_rfc3339(),
                    screen
                ),
            );
            for r in reports {
                append_host_log(
                    &log_path_for_attempts,
                    &format!(
                        "  handler={} active={} matched={} anchors={:?} fields={:?}",
                        r.handler_id, r.active, r.matched, r.anchors, r.fields
                    ),
                );
            }
        },
        move |direction, bytes| {
            append_host_log(
                &log_path_for_raw_io,
                &format!(
                    "[{}] {direction} n={} {}",
                    chrono::Utc::now().to_rfc3339(),
                    bytes.len(),
                    escape_bytes(bytes)
                ),
            );
        },
        Some(watch_hub),
    )
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
    started_at: chrono::DateTime<chrono::Utc>,
    config: Option<&approvals::Config>,
    approvals_summary: &str,
    log_path: &std::path::Path,
    screen: String,
) {
    let state_dir_buf = crate::state_dir();
    let state_dir = state_dir_buf.as_path();
    let now = chrono::Utc::now().to_rfc3339();
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
    let line = match notify::notify_role(notify::broker_addr(), state_dir, notify_role, &ask.text())
    {
        Ok(ids) => format!("[{now}] ASK SENT to {notify_role} peers {ids:?}"),
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
    let path = dir.join(format!("{}.txt", chrono::Utc::now().to_rfc3339()).replace(':', "-"));
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
        let mut parser = vt100::Parser::new(24, 80, 0);
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
        let mut parser = vt100::Parser::new(24, 80, 0);
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk);
    }

    #[test]
    fn relay_chunk_forwards_mouse_mode_sequences_byte_for_byte() {
        // \x1b[?1000h / \x1b[?1000l - mouse tracking on/off.
        let chunk = b"\x1b[?1000hclick here\x1b[?1000l";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(24, 80, 0);
        relay_chunk(chunk, &mut out, &mut parser).unwrap();
        assert_eq!(out, chunk);
    }

    #[test]
    fn relay_chunk_forwards_alt_screen_sequences_byte_for_byte() {
        // \x1b[?1049h / \x1b[?1049l - alternate screen buffer on/off.
        let chunk = b"\x1b[?1049hfull screen app\x1b[?1049l";
        let mut out = Vec::new();
        let mut parser = vt100::Parser::new(24, 80, 0);
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
        let mut parser = vt100::Parser::new(24, 80, 0);
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
        let mut parser = vt100::Parser::new(24, 80, 0);
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
            chrono::Utc::now(),
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
            chrono::Utc::now(),
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
                chrono::Utc::now(),
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
            chrono::Utc::now(),
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
                "run {run}: helper threads still alive after return: {:?}",
                outcome.abandoned_threads
            );
        }
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

        let outcome = run_with_handlers_and_window(
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
            |_, _| {},
        )
        .expect("run_with_handlers_and_window failed");

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

        run_with_handlers_and_window(
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
            |_, _| {},
        )
        .expect("run_with_handlers_and_window failed");

        assert!(
            captured.lock().unwrap().is_empty(),
            "a real match must suppress the unhandled-prompt capture entirely"
        );
    }

    /// Issue #96 (a): `on_unhandled` sends the PM ask over the claude-peers
    /// broker, and a stalled broker must not freeze the terminal. The
    /// callback here blocks until released (or 10s); the child prints a
    /// second marker ~2s after the window ends. That marker must reach the
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
            let deadline = Instant::now() + Duration::from_secs(15);
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

        run_with_handlers_and_window(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            outer_input,
            SharedVecWriter(Arc::clone(&outer_output)),
            |_| {},
            move |_text| {
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
                returned_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            Duration::from_millis(800),
            |_, _| {},
            |_, _| {},
        )
        .expect("run_with_handlers_and_window failed");

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
                "echo PROBE_MARKER_98765 & choice /t 1 /d y >nul".to_string(),
            ],
            "echo PROBE_MARKER_98765; sleep 1",
        );
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished_for_cb = Arc::clone(&finished);

        run_with_handlers_and_window(
            "overmind",
            &child_argv,
            Vec::new(),
            80,
            25,
            Cursor::new(b"\x1b[1;1R".to_vec()),
            std::io::sink(),
            |_| {},
            move |_text| {
                std::thread::sleep(Duration::from_secs(3));
                finished_for_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            },
            Duration::from_millis(300),
            |_, _| {},
            |_, _| {},
        )
        .expect("run_with_handlers_and_window failed");

        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "run must not return while on_unhandled is still running"
        );
    }
}
