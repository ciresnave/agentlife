// SPDX-License-Identifier: MIT OR Apache-2.0
//! `gpu-run -Project <name> -- <cmd> <args...>`: baracuda/fuel `scripts/gpu-run.ps1` (v3) as a native
//! exe. Same mutex (`Global\gpu-run`, same ACL), same lockfile and log, same nesting variables, so it
//! interleaves with the PowerShell copies. Windows only.

#[cfg(not(windows))]
fn main() {
    eprintln!("gpu-run: Windows only (the Linux entry point is scripts/gpu-run.sh)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::run());
}

#[cfg(windows)]
mod win {
    use gpu_run::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::ptr::null_mut;
    use std::time::{Instant, SystemTime};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Threading::{
        CreateMutexW, GetExitCodeProcess, OpenMutexW, OpenProcess, ReleaseMutex,
        WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const MUTEX_MODIFY_STATE: u32 = 0x0001;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_ABANDONED: u32 = 0x80;
    const WAIT_TIMEOUT: u32 = 0x102;
    const STILL_ACTIVE: u32 = 259;

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    struct Ctx {
        project: String,
        pid: u32,
        log: PathBuf,
        lock: PathBuf,
    }

    impl Ctx {
        /// Best effort; a logging failure never aborts or wedges a GPU run.
        fn log(&self, msg: &str) {
            use std::io::Write;
            let line = log_line(&iso_utc(SystemTime::now()), &self.project, self.pid, msg);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log)
            {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// Open `Global\gpu-run` with only the rights Wait/Release need; create it (ACL: Authenticated
    /// Users get Synchronize|Modify) only when absent. Same order as `gpu-run-mutex.ps1`.
    fn open_or_create() -> Result<HANDLE, String> {
        let name = wide(MUTEX_NAME);
        for _ in 0..8 {
            // SAFETY: plain Win32 calls on a NUL-terminated name.
            unsafe {
                let h = OpenMutexW(SYNCHRONIZE | MUTEX_MODIFY_STATE, 0, name.as_ptr());
                if !h.is_null() {
                    return Ok(h);
                }
                match GetLastError() {
                    ERROR_FILE_NOT_FOUND => {}
                    ERROR_ACCESS_DENIED => {
                        return Err(format!(
                            "gpu-run: cannot open '{MUTEX_NAME}' (access denied). It already exists with an ACL that \
                             excludes this account (an older gpu-run created it with the default ACL). Let that holder \
                             finish, or stop it, so the mutex is destroyed; the next gpu-run recreates it open to \
                             Authenticated Users."
                        ));
                    }
                    e => return Err(format!("gpu-run: OpenMutex failed, error {e}")),
                }
                // AU = Authenticated Users; 0x100001 = SYNCHRONIZE | MUTEX_MODIFY_STATE.
                let sddl = wide("D:(A;;0x100001;;;AU)");
                let mut sd = null_mut();
                if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    null_mut(),
                ) == 0
                {
                    return Err(format!(
                        "gpu-run: building the mutex ACL failed, error {}",
                        GetLastError()
                    ));
                }
                let sa = SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd,
                    bInheritHandle: 0,
                };
                let h = CreateMutexW(&sa, 0, name.as_ptr());
                let err = GetLastError();
                LocalFree(sd as _);
                if !h.is_null() {
                    // ERROR_ALREADY_EXISTS (183): someone created it between our open and create; theirs
                    // is the one that counts, and CreateMutex returned a handle to it.
                    let _ = err;
                    return Ok(h);
                }
            }
        }
        Err(format!(
            "gpu-run: could not open or create '{MUTEX_NAME}' after 8 attempts"
        ))
    }

    fn pid_alive(pid: u32) -> bool {
        // SAFETY: plain Win32 calls; the handle is closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok == 0 || code == STILL_ACTIVE
        }
    }

    fn spawn(command: &[String], held_pid: Option<u32>) -> i32 {
        let mut c = Command::new(&command[0]);
        c.args(&command[1..]);
        if let Some(p) = held_pid {
            c.env("GPU_RUN_HELD", "1")
                .env("GPU_RUN_HELD_PID", p.to_string());
        }
        match c.status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("gpu-run: cannot start '{}': {e}", command[0]);
                127
            }
        }
    }

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let inv = match parse_args(&args) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        let temp = std::env::var_os("TEMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let ctx = Ctx {
            project: inv.project.clone(),
            pid: std::process::id(),
            log: temp.join("gpu-run.log"),
            lock: temp.join("gpu-run.lock"),
        };
        let cmd_str = inv.command.join(" ");

        // Nesting: already under the lock on this process tree.
        if std::env::var("GPU_RUN_HELD").as_deref() == Ok("1") {
            let by = std::env::var("GPU_RUN_HELD_PID").unwrap_or_default();
            ctx.log(&format!(
                "nested gpu-run — passing through (lock already held by pid={by}): {cmd_str}"
            ));
            return spawn(&inv.command, None);
        }

        let mutex = match open_or_create() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        let t0 = Instant::now();
        let holder_before = std::fs::read_to_string(&ctx.lock).ok();
        let mut abandoned = false;
        // SAFETY: `mutex` is a valid handle owned by this thread for the whole run.
        unsafe {
            match WaitForSingleObject(mutex, 0) {
                WAIT_OBJECT_0 => {}
                WAIT_ABANDONED => abandoned = true,
                _ => {
                    ctx.log(&format!(
                        "WAIT — GPU lock held by: {}",
                        holder_before.as_deref().unwrap_or("")
                    ));
                    let timeout_min =
                        timeout_minutes(std::env::var("GPU_RUN_TIMEOUT_MIN").ok().as_deref());
                    loop {
                        match WaitForSingleObject(mutex, 60_000) {
                            WAIT_OBJECT_0 => break,
                            WAIT_ABANDONED => {
                                abandoned = true;
                                break;
                            }
                            WAIT_TIMEOUT => {
                                let mins = t0.elapsed().as_secs() / 60;
                                let holder_now = std::fs::read_to_string(&ctx.lock)
                                    .unwrap_or_else(|_| "(lockfile gone)".to_string());
                                let warn = format!(
                                    "gpu-run: still WAITING {mins}m for the GPU lock — holder: {holder_now}"
                                );
                                eprintln!("{warn}");
                                ctx.log(&warn);
                                if mins >= timeout_min {
                                    let abort = format!(
                                        "gpu-run: ABORT after {mins}m (>= {timeout_min}m) — holder appears WEDGED \
                                         (a hung holder never abandons the mutex). Holder: {holder_now}. \
                                         Investigate/kill it (Stop-Process), then retry; raise the bound with \
                                         GPU_RUN_TIMEOUT_MIN."
                                    );
                                    eprintln!("{abort}");
                                    ctx.log(&abort);
                                    CloseHandle(mutex);
                                    return EXIT_WEDGED;
                                }
                            }
                            other => {
                                eprintln!(
                                    "gpu-run: wait failed ({other:#x}, error {})",
                                    GetLastError()
                                );
                                CloseHandle(mutex);
                                return 1;
                            }
                        }
                    }
                }
            }
        }
        if abandoned {
            ctx.log(&format!(
                "RECLAIMED abandoned GPU lock (previous holder died mid-hold). Stale lockfile: {}",
                holder_before.as_deref().unwrap_or("")
            ));
        }
        let waited = t0.elapsed().as_secs();
        if holder_before.is_some() && waited >= 1 {
            ctx.log(&format!("acquired after {waited}s wait"));
        }
        // Uncontended crash: the mutex object died with its last handle, only the lockfile remembers.
        if let Some(h) = &holder_before {
            if let Some(stale) = lock_pid(h) {
                if !pid_alive(stale) {
                    let msg = format!(
                        "RECLAIMED stale GPU lock from DEAD pid={stale} (uncontended crash - no \
                         AbandonedMutexException is raised when the mutex object dies with its last \
                         handle). Stale lockfile: {h}"
                    );
                    eprintln!("gpu-run: {msg}");
                    ctx.log(&msg);
                }
            }
        }
        let start = iso_utc(SystemTime::now());
        let _ = std::fs::write(
            &ctx.lock,
            lock_json(ctx.pid, &inv.project, &cmd_str, &start),
        );
        ctx.log(&format!("RUN: {cmd_str}"));

        let code = spawn(&inv.command, Some(ctx.pid));
        ctx.log(&format!("DONE exit={code} : {cmd_str}"));

        let _ = std::fs::remove_file(&ctx.lock);
        // SAFETY: same thread that acquired it.
        unsafe {
            ReleaseMutex(mutex);
            CloseHandle(mutex);
        }
        ctx.log("released");
        code
    }
}
