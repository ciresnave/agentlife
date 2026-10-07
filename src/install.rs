// SPDX-License-Identifier: MIT OR Apache-2.0
//! Installing a binary that is in use: **rename, never overwrite**.
//!
//! The hook binary is run by every Claude Code session (`PreToolUse` alone fires per tool call in
//! `lane-restart`'s case), so a plain copy over it is refused by Windows while any session has it
//! open. OverMind found this on its real install (`RESTART-TOOL-DESIGN.md` §11.2). The sequence:
//!
//! 1. copy the new binary to `<dest>.new`;
//! 2. rename the live `<dest>` to `<dest>.old` (Windows lets an in-use file be renamed away);
//! 3. rename `<dest>.new` to `<dest>`.
//!
//! A hook invocation already running keeps the old file; the next one resolves the path to the new
//! one. If step 3 fails, step 2 is **rolled back**, so the destination is never left missing.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    /// Where the previous binary now is, if there was one.
    pub backup: Option<PathBuf>,
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// [`install_by_rename`] with the rename operation injected, so the rollback is testable.
pub fn install_with(
    new: &Path,
    dest: &Path,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<Installed> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged = with_suffix(dest, ".new");
    let old = with_suffix(dest, ".old");
    std::fs::copy(new, &staged)?;

    let had_dest = dest.exists();
    if had_dest {
        // A previous backup may itself still be in use; failing to remove it is not fatal, the
        // rename below simply replaces it if the OS allows and errors out cleanly if not.
        let _ = std::fs::remove_file(&old);
        if let Err(e) = rename(dest, &old) {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
    }
    if let Err(e) = rename(&staged, dest) {
        if had_dest {
            // Roll back so `dest` is never left missing.
            let _ = std::fs::rename(&old, dest);
        }
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(Installed {
        backup: had_dest.then_some(old),
    })
}

/// Installs `new` at `dest` by the rename sequence above.
pub fn install_by_rename(new: &Path, dest: &Path) -> io::Result<Installed> {
    install_with(new, dest, &|a, b| std::fs::rename(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_fresh_install_has_no_backup() {
        let d = tempfile::tempdir().unwrap();
        let new = d.path().join("new.bin");
        std::fs::write(&new, b"v2").unwrap();
        let dest = d.path().join("bin").join("agentlife.exe");
        let r = install_by_rename(&new, &dest).unwrap();
        assert_eq!(r, Installed { backup: None });
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2");
        assert!(
            !with_suffix(&dest, ".new").exists(),
            "the staged copy is gone"
        );
    }

    #[test]
    fn replacing_keeps_the_previous_binary_as_dot_old() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("agentlife.exe");
        std::fs::write(&dest, b"v1").unwrap();
        let new = d.path().join("new.bin");
        std::fs::write(&new, b"v2").unwrap();
        let r = install_by_rename(&new, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"v2");
        let backup = r.backup.expect("there was a binary to back up");
        assert_eq!(std::fs::read(&backup).unwrap(), b"v1");
        // A second install replaces the old backup rather than failing.
        std::fs::write(&new, b"v3").unwrap();
        install_by_rename(&new, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"v3");
        assert_eq!(std::fs::read(with_suffix(&dest, ".old")).unwrap(), b"v2");
    }

    #[test]
    fn a_failed_final_rename_rolls_back_so_the_destination_is_never_missing() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("agentlife.exe");
        std::fs::write(&dest, b"v1").unwrap();
        let new = d.path().join("new.bin");
        std::fs::write(&new, b"v2").unwrap();
        let calls = Cell::new(0);
        let r = install_with(&new, &dest, &|a, b| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(io::Error::other("injected: the final rename fails"))
            } else {
                std::fs::rename(a, b)
            }
        });
        assert!(r.is_err());
        assert_eq!(
            calls.get(),
            2,
            "the failure was injected on the second rename"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"v1",
            "the old binary is back in place"
        );
        assert!(
            !with_suffix(&dest, ".new").exists(),
            "no staged copy left behind"
        );
    }

    #[test]
    fn a_failed_first_rename_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("agentlife.exe");
        std::fs::write(&dest, b"v1").unwrap();
        let new = d.path().join("new.bin");
        std::fs::write(&new, b"v2").unwrap();
        let r = install_with(&new, &dest, &|_, _| Err(io::Error::other("injected")));
        assert!(r.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"v1");
        assert!(!with_suffix(&dest, ".new").exists());
    }

    /// The reason this module exists: on Windows a running executable cannot be overwritten, but
    /// it can be renamed. Proven with a real running process, not described.
    #[cfg(windows)]
    #[test]
    fn a_running_executable_cannot_be_overwritten_but_can_be_replaced_by_rename() {
        let sys32 =
            std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or("C:\\Windows".into()))
                .join("System32");
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("live.exe");
        std::fs::copy(sys32.join("PING.EXE"), &dest).unwrap();
        let new = d.path().join("new.exe");
        std::fs::copy(sys32.join("HOSTNAME.EXE"), &new).unwrap();
        let new_bytes = std::fs::read(&new).unwrap();
        let old_bytes = std::fs::read(&dest).unwrap();

        let mut child = std::process::Command::new(&dest)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("start the copied executable");
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Positive control: the plain overwrite really is refused while it runs.
        assert!(
            std::fs::copy(&new, &dest).is_err(),
            "overwriting a running executable should be refused on Windows"
        );
        // The rename sequence succeeds on the same running file.
        let r = install_by_rename(&new, &dest).expect("rename-install over a running exe");
        assert_eq!(std::fs::read(&dest).unwrap(), new_bytes);
        assert_eq!(std::fs::read(r.backup.unwrap()).unwrap(), old_bytes);

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
