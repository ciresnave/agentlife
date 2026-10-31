// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart attach --watch`: a read-only view of a running lane (`docs/PORT-LINUX-PLAN.md`,
//! slice P6b; approved by the PM 2026-10-14 as the only attach to build).
//!
//! A direct host (P6a) has no terminal, so a person cannot see a lane stuck at a dialog. The host
//! already owns the pty and keeps a `vt100` screen model. This module lets it show both to a
//! watcher over a unix-domain socket:
//!
//! * the watcher first receives the current screen (`contents_formatted`), then every chunk the
//!   child writes afterwards, in order and without a gap;
//! * **nothing ever flows the other way.** The server never reads from the socket into the child:
//!   the host's own stdin stays the only way in. A watcher that sends bytes has them ignored;
//! * a slow watcher is dropped, never waited for: the relay's pace is the child's, not a viewer's.
//!
//! ⚠️ Interactive (write) attach is NOT built and not approved: a `0600` socket is the same trust
//! boundary the pty already has, and does not by itself keep a same-user agent off it
//! (`PORT-LINUX-PLAN.md` P6c). Because the server has no input path at all, there is nothing to
//! misconfigure.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

/// How many unread chunks a watcher may fall behind before it is dropped.
pub const WATCHER_BACKLOG: usize = 256;

/// What a new watcher gets: the screen now, and the live chunks that follow it.
pub struct Subscription {
    pub snapshot: Vec<u8>,
    pub live: Receiver<Vec<u8>>,
}

type Request = SyncSender<Subscription>;

/// Fan-out from the relay loop to any number of watchers. Platform-neutral and socket-free, so it
/// is tested without a socket; the relay loop calls [`WatchHub::service`] and [`WatchHub::publish`].
#[derive(Default)]
pub struct WatchHub {
    watchers: Mutex<Vec<SyncSender<Vec<u8>>>>,
    requests: Mutex<Vec<Request>>,
}

impl WatchHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called by a watcher's connection thread: asks the relay loop for a subscription and waits for
    /// it. `None` if the relay loop did not answer in time (the host is gone or stalled).
    pub fn subscribe(&self, wait: Duration) -> Option<Subscription> {
        let (tx, rx) = sync_channel::<Subscription>(1);
        self.requests.lock().ok()?.push(tx);
        rx.recv_timeout(wait).ok()
    }

    /// Called by the relay loop, between chunks: answers pending requests with `screen()` taken at
    /// this exact point, and registers each watcher in the same step, so what follows is exactly
    /// the chunks that arrive after the snapshot.
    pub fn service(&self, screen: impl Fn() -> Vec<u8>) {
        let pending: Vec<Request> = match self.requests.lock() {
            Ok(mut r) if !r.is_empty() => std::mem::take(&mut *r),
            _ => return,
        };
        let snapshot = screen();
        let Ok(mut watchers) = self.watchers.lock() else {
            return;
        };
        for reply in pending {
            let (tx, rx) = sync_channel::<Vec<u8>>(WATCHER_BACKLOG);
            if reply
                .try_send(Subscription {
                    snapshot: snapshot.clone(),
                    live: rx,
                })
                .is_ok()
            {
                watchers.push(tx);
            }
        }
    }

    /// Called by the relay loop for every chunk the child wrote. Never blocks: a watcher that is
    /// full or gone is dropped.
    pub fn publish(&self, chunk: &[u8]) {
        let Ok(mut watchers) = self.watchers.lock() else {
            return;
        };
        if watchers.is_empty() {
            return;
        }
        watchers.retain(|w| match w.try_send(chunk.to_vec()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// Watchers currently registered.
    pub fn watcher_count(&self) -> usize {
        self.watchers.lock().map_or(0, |w| w.len())
    }
}

/// The bytes that put a watcher's terminal in the host's screen state: clear, then the formatted
/// contents of the `vt100` screen.
pub fn snapshot_of(parser: &vt100::Parser) -> Vec<u8> {
    let mut out = b"\x1b[2J\x1b[H".to_vec();
    out.extend_from_slice(parser.screen().contents_formatted().as_bytes());
    out
}

#[cfg(unix)]
pub use unix::{connect_and_copy, serve, socket_path, Server};

#[cfg(unix)]
mod unix {
    use super::WatchHub;
    use std::io::{Read, Write};
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// `<state>/watch/host-<role>.sock`. The directory is created `0700` BEFORE the socket is bound
    /// inside it, so there is no window in which the socket is reachable by another user.
    pub fn socket_path(state_dir: &Path, role: &str) -> PathBuf {
        state_dir.join("watch").join(format!("host-{role}.sock"))
    }

    /// A running watch server. Dropping it (or calling [`Server::stop`]) stops accepting and
    /// removes the socket.
    pub struct Server {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        path: PathBuf,
    }

    impl Server {
        pub fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            let _ = std::fs::remove_file(&self.path);
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Binds `path` and serves watchers from `hub` until the returned [`Server`] is stopped.
    /// ⚠️ The connection handler only ever WRITES to the stream; see the module note.
    pub fn serve(hub: Arc<WatchHub>, path: PathBuf) -> std::io::Result<Server> {
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        // a stale socket from a host that died would make `bind` fail
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stop_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let hub = Arc::clone(&hub);
                        let stop = Arc::clone(&stop_for_thread);
                        std::thread::spawn(move || handle(stream, &hub, &stop));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Server {
            stop,
            thread: Some(thread),
            path,
        })
    }

    fn handle(mut stream: UnixStream, hub: &WatchHub, stop: &AtomicBool) {
        // a viewer that stops reading must not hold this thread forever
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let Some(sub) = hub.subscribe(Duration::from_secs(3)) else {
            return;
        };
        if stream.write_all(&sub.snapshot).is_err() {
            return;
        }
        while !stop.load(Ordering::SeqCst) {
            match sub.live.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => {
                    if stream.write_all(&chunk).is_err() {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// The `attach --watch` client: connects to `path` and copies what the host sends to `out`
    /// until the host closes it. Sends nothing.
    pub fn connect_and_copy(path: &Path, out: &mut dyn Write) -> std::io::Result<()> {
        let mut stream = UnixStream::connect(path)?;
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf)? {
                0 => return Ok(()),
                n => {
                    out.write_all(&buf[..n])?;
                    out.flush()?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(rx: &Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
        rx.try_iter().collect()
    }

    /// The subscription is answered by the relay loop (`service`), and what follows the snapshot
    /// is exactly the chunks published after it.
    #[test]
    fn a_watcher_gets_the_screen_then_only_what_came_after() {
        let hub = std::sync::Arc::new(WatchHub::new());
        hub.publish(b"before any watcher");
        let h = std::sync::Arc::clone(&hub);
        let t = std::thread::spawn(move || h.subscribe(Duration::from_secs(5)));
        // the relay loop services requests between chunks
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hub.watcher_count() == 0 && std::time::Instant::now() < deadline {
            hub.service(|| b"SCREEN".to_vec());
            std::thread::sleep(Duration::from_millis(10));
        }
        let sub = t.join().unwrap().expect("a subscription");
        assert_eq!(sub.snapshot, b"SCREEN");
        assert!(chunks(&sub.live).is_empty(), "nothing before the snapshot");
        hub.publish(b"one");
        hub.publish(b"two");
        assert_eq!(chunks(&sub.live), vec![b"one".to_vec(), b"two".to_vec()]);
    }

    /// Control: with no watcher, publishing is a no-op and costs nothing.
    #[test]
    fn publishing_with_no_watcher_does_nothing() {
        let hub = WatchHub::new();
        hub.publish(b"x");
        assert_eq!(hub.watcher_count(), 0);
        hub.service(|| panic!("no request pending: the screen must not be taken"));
    }

    /// A watcher that falls behind is dropped, and the relay is never blocked by it.
    #[test]
    fn a_slow_watcher_is_dropped_and_publish_never_blocks() {
        let hub = std::sync::Arc::new(WatchHub::new());
        let h = std::sync::Arc::clone(&hub);
        let t = std::thread::spawn(move || h.subscribe(Duration::from_secs(5)));
        while hub.watcher_count() == 0 {
            hub.service(Vec::new);
            std::thread::sleep(Duration::from_millis(5));
        }
        let _sub = t.join().unwrap().unwrap();
        // never read: the backlog fills, then the watcher is removed
        for _ in 0..(WATCHER_BACKLOG + 5) {
            hub.publish(b"chunk");
        }
        assert_eq!(hub.watcher_count(), 0);
    }

    #[test]
    fn a_disconnected_watcher_is_dropped_on_the_next_publish() {
        let hub = std::sync::Arc::new(WatchHub::new());
        let h = std::sync::Arc::clone(&hub);
        let t = std::thread::spawn(move || h.subscribe(Duration::from_secs(5)));
        while hub.watcher_count() == 0 {
            hub.service(Vec::new);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(t.join().unwrap().unwrap());
        hub.publish(b"x");
        assert_eq!(hub.watcher_count(), 0);
    }

    #[test]
    fn the_snapshot_is_a_clear_then_the_screen_contents() {
        let mut p = vt100::Parser::new(
            std::num::NonZeroU16::new(5).unwrap(),
            std::num::NonZeroU16::new(20).unwrap(),
            0,
        );
        p.process(b"hello\r\nworld");
        let s = snapshot_of(&p);
        assert!(s.starts_with(b"\x1b[2J\x1b[H"));
        let text = String::from_utf8_lossy(&s);
        assert!(text.contains("hello") && text.contains("world"), "{text:?}");
    }

    #[cfg(unix)]
    mod socket {
        use super::*;
        use std::io::{Read, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;

        /// Real socket: the watcher sees the snapshot then live output; the socket and its
        /// directory are not reachable by group or others.
        #[test]
        fn a_watcher_over_the_socket_sees_the_screen_and_live_output() {
            let dir = tempfile::tempdir().unwrap();
            let path = socket_path(dir.path(), "r");
            let hub = std::sync::Arc::new(WatchHub::new());
            let server = serve(std::sync::Arc::clone(&hub), path.clone()).unwrap();

            let dmode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dmode & 0o077, 0, "watch dir must be 0700, was {dmode:o}");

            let mut c = UnixStream::connect(&path).unwrap();
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            // the relay loop: service, then publish
            let hub2 = std::sync::Arc::clone(&hub);
            let relay = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while hub2.watcher_count() == 0 && std::time::Instant::now() < deadline {
                    hub2.service(|| b"SNAPSHOT".to_vec());
                    std::thread::sleep(Duration::from_millis(10));
                }
                hub2.publish(b"LIVE");
            });
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while !String::from_utf8_lossy(&got).contains("LIVE") {
                let n = c.read(&mut buf).unwrap();
                assert!(n > 0, "closed early: {:?}", String::from_utf8_lossy(&got));
                got.extend_from_slice(&buf[..n]);
            }
            relay.join().unwrap();
            let text = String::from_utf8_lossy(&got).to_string();
            assert!(text.starts_with("SNAPSHOT"), "{text:?}");
            assert!(text.ends_with("LIVE"), "{text:?}");
            drop(c);
            drop(server);
            assert!(
                !path.exists(),
                "the socket is removed when the server stops"
            );
        }

        /// Bytes a watcher sends go nowhere: the server has no read path, and the hub has no
        /// input at all.
        #[test]
        fn bytes_a_watcher_sends_are_not_read_by_the_server() {
            let dir = tempfile::tempdir().unwrap();
            let path = socket_path(dir.path(), "r");
            let hub = std::sync::Arc::new(WatchHub::new());
            let _server = serve(std::sync::Arc::clone(&hub), path.clone()).unwrap();
            let mut c = UnixStream::connect(&path).unwrap();
            c.write_all(b"RM -RF /\n").unwrap();
            // the connection is still served normally afterwards
            let hub2 = std::sync::Arc::clone(&hub);
            let relay = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while hub2.watcher_count() == 0 && std::time::Instant::now() < deadline {
                    hub2.service(|| b"S".to_vec());
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap(), 1);
            relay.join().unwrap();
            assert_eq!(&b, b"S");
        }
    }
}
