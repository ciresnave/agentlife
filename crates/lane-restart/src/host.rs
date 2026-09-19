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

/// Checks `screen_text` against every active, embedded handler for `role`;
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
            input_writer.write_all(handler.action.as_bytes())?;
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
    mut outer_input: impl Read + Send + 'static,
    mut outer_output: impl Write + Send + 'static,
    on_answered: impl FnMut(HandlerAnswered) + Send + 'static,
    on_unhandled: impl FnOnce(String) + Send + 'static,
    startup_window: Duration,
    mut on_match_attempt: impl FnMut(&str, &[handlers::MatchReport]) + Send + 'static,
    on_raw_io: impl Fn(&str, &[u8]) + Send + Sync + 'static,
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
    {
        let pty_writer = Arc::clone(&pty_writer);
        let on_raw_io = Arc::clone(&on_raw_io);
        std::thread::spawn(move || {
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
    {
        let master_for_resize = pair.master;
        let mut last = (cols, rows);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
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
        });
    }

    // ⚠️ THE CPR-BLOCKING INVESTIGATION'S OTHER FINDING (2026-09-18,
    // scratch-probe iteration 4): ConPTY can keep its own internal
    // reference to the pipe's write end alive even after the hosted child
    // exits, so a blocking `read` in THIS thread can hang forever past
    // exit - a plain "loop until read returns 0" never terminates in that
    // case. Fixed the same way the probe was: the blocking read lives on
    // its own thread, sending chunks over a channel; this thread polls
    // that channel with a short timeout and separately checks
    // `child.try_wait()` for the real exit signal, abandoning (never
    // joining) the reader thread once the child is confirmed gone - it may
    // still be stuck in `ReadFile` forever, and that's fine to leave
    // behind for the rest of the process's own life.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    {
        let on_raw_io = Arc::clone(&on_raw_io);
        std::thread::spawn(move || {
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
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut matched_ids: HashSet<String> = HashSet::new();
    let mut last_logged_screen: Option<String> = None;
    let started = Instant::now();
    loop {
        let still_in_startup_window = started.elapsed() < startup_window;
        if !still_in_startup_window {
            if let Some(capture) = on_unhandled.take() {
                if matched_ids.is_empty() {
                    capture(screen_text(&parser));
                }
            }
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                relay_chunk(&chunk, &mut outer_output, &mut parser)?;
                on_raw_io("relay->outer_stdout", &chunk);
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
    Ok(HostOutcome {
        child_exit_code: status.and_then(|s| s.exit_code().into()),
    })
}

/// `.lane-state/host-<role>-<pid>.log` - PM finding, 2026-09-19: a real
/// restart left NOTHING to explain a handler that should have matched but
/// didn't, so this diagnostic log exists purely to make the NEXT run
/// explain itself: the screen text at each real change, the per-anchor and
/// per-field match result for every embedded handler, and any injection
/// with the exact bytes sent.
fn host_log_path(role: &str) -> std::path::PathBuf {
    std::path::Path::new("C:/Projects/.lane-state")
        .join(format!("host-{role}-{}.log", std::process::id()))
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

/// Convenience entry point for real production use: loads the embedded
/// handlers (§12.3) and wires this process's OWN real stdin/stdout as the
/// outer relay ends.
pub fn run(role: &str, child_argv: &[String]) -> std::io::Result<HostOutcome> {
    // ⚠️ Kept alive for `run`'s ENTIRE scope - restored on every exit path
    // via `Drop`, including a panic unwind.
    #[cfg(windows)]
    let _raw_console_guard = RawConsoleGuard::enable();

    let (cols, rows) = current_console_size().unwrap_or((80, 25));
    let log_path = host_log_path(role);
    let log_path_for_answered = log_path.clone();
    let log_path_for_attempts = log_path.clone();
    let log_path_for_raw_io = log_path.clone();
    run_with_handlers_and_window(
        role,
        child_argv,
        handlers::load_embedded_handlers(),
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
        capture_unhandled_prompt,
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

/// RESTART-TOOL-DESIGN.md §12.6: writes the captured screen text verbatim
/// to `.lane-state/unhandled-prompts/<timestamp>.txt` - the fixed,
/// portfolio-wide state directory `main.rs`'s own `state_dir()` uses, not
/// configurable here either, for the same reason: a caller-supplied
/// location would defeat the point of every lane and the PM agreeing on
/// one place to look. Best-effort: a write failure is reported to stderr,
/// never allowed to take down the relay that's still running.
fn capture_unhandled_prompt(screen_text: String) {
    let dir = std::path::Path::new("C:/Projects/.lane-state/unhandled-prompts");
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("lane-restart host: could not create {dir:?}: {e}");
        return;
    }
    let path = dir.join(format!("{}.txt", chrono::Utc::now().to_rfc3339()).replace(':', "-"));
    if let Err(e) = std::fs::write(&path, screen_text) {
        eprintln!("lane-restart host: could not write {path:?}: {e}");
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

#[cfg(not(windows))]
pub fn current_console_size() -> Option<(u16, u16)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::io::Cursor;

    // -- relay_chunk: the transparency property itself --------------------- //
    // PM finding, 2026-09-18: the host must be a transparent relay from
    // byte 0 - matching only OBSERVES, it never withholds, delays, or
    // rewrites a single byte, including mouse-mode and alt-screen
    // sequences (not just plain text).

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
                "provenance": {{"approved_by": "CireSnave", "approved_at": "2026-09-18T22:00:00Z", "quote": "q"}}
            }}"#
        )
    }

    #[test]
    fn check_and_inject_writes_the_action_on_an_exact_match() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\n")).unwrap();
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
        assert_eq!(input, b"y\n");
        assert_eq!(answered, vec!["h1".to_string()]);
    }

    #[test]
    fn check_and_inject_never_fires_the_same_handler_twice() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\n")).unwrap();
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
        assert_eq!(input, b"y\n", "the action must be written exactly once");
        assert_eq!(answered.len(), 1);
    }

    #[test]
    fn check_and_inject_does_nothing_when_no_handler_matches() {
        let handler: HandlerSpec =
            serde_json::from_str(&handler_json("h1", "CONFIRM DIALOG", "y\\n")).unwrap();
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

    #[cfg(windows)]
    #[test]
    fn raw_console_guard_enable_and_drop_never_panics() {
        let guard = RawConsoleGuard::enable();
        drop(guard);
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
    #[cfg(windows)]
    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    #[cfg(windows)]
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
    #[cfg(windows)]
    struct ChanReader {
        rx: std::sync::mpsc::Receiver<Vec<u8>>,
        pending: Vec<u8>,
        pos: usize,
    }
    #[cfg(windows)]
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

    #[cfg(windows)]
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

        let child_argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765".to_string(),
        ];
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

    #[cfg(windows)]
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

        let child_argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765".to_string(),
        ];
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

    #[cfg(windows)]
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

        let child_argv = vec!["cmd.exe".to_string(), "/c".to_string(), "cd".to_string()];
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

    #[cfg(windows)]
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
            "INJECTED_REPLY\\r\\n",
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

        // ⚠️ `%REPLY%` expands at PARSE time, before `set /p` ever runs, so
        // it would stay the literal text `%REPLY%` regardless of what's
        // typed - `/v:on` (delayed expansion, from the start of THIS
        // invocation, unlike `setlocal enabledelayedexpansion` mid-line
        // which is still too late inside one `&`-chained command line) plus
        // `!REPLY!` is what makes this actually prove the injected text was
        // received.
        let child_argv = vec![
            "cmd.exe".to_string(),
            "/v:on".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765 & set /p REPLY=waiting: & echo GOT:!REPLY!".to_string(),
        ];
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
            text.contains("GOT:INJECTED_REPLY"),
            "the real child must have received the real injected keystrokes \
             back on its own stdin - got {text:?}"
        );
        assert!(outcome.child_exit_code.is_some());
    }

    // -- on_unhandled: §12.6 unhandled-prompt capture --------------------- //
    // A tiny `startup_window` (via `run_with_handlers_and_window`, not the
    // real 60s `STARTUP_WINDOW`) is what makes these fast without waiting
    // it out for real.

    #[cfg(windows)]
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

        // No handler is embedded (`Vec::new()` below), so nothing will ever
        // inject a reply - `choice` is used instead of `set /p` so the
        // child still terminates ON ITS OWN (auto-selects after 2s)
        // rather than blocking forever on input that will never arrive.
        let child_argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765 & choice /t 2 /d y >nul".to_string(),
        ];
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_for_cb = Arc::clone(&captured);

        let outcome = run_with_handlers_and_window(
            "overmind",
            &child_argv,
            Vec::new(), // no handlers embedded - nothing can ever match
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

    #[cfg(windows)]
    #[test]
    fn on_unhandled_never_fires_when_a_handler_matched_during_the_window() {
        let handler: HandlerSpec = serde_json::from_str(&handler_json(
            "echo-responder",
            "PROBE_MARKER_98765",
            "REPLY\\r\\n",
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

        let child_argv = vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo PROBE_MARKER_98765 & set /p REPLY=waiting: & echo done".to_string(),
        ];
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
}
