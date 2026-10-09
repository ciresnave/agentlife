// SPDX-License-Identifier: MIT OR Apache-2.0
//! Write a file so a reader never sees half of it, through `persistant`'s blocking `fs` store:
//! it writes a temp file in `atomic_write_dir`, syncs it, then renames it over the target.
//!
//! `persistant` requires that scratch directory to be on the target's filesystem and outside
//! its root, and it leaves an abandoned write's temp file behind (OpenDAL has no cleanup on
//! drop). So each call gets its own scratch directory, a sibling of the target's directory
//! (`<dir>.tmp/<pid>-<n>`), and removes it before returning, whatever the outcome: nothing
//! shares it, so nothing can be swept from under a live writer. A crash mid-write can leave
//! that directory behind, but never inside the directory a lister reads.
//!
//! [`is_temp_name`] still recognises the `.<name>.tmp.<pid>.<n>` files an earlier version
//! wrote beside their target, so a lister skips any that survive from before.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use persistant::blocking::Store;
use persistant::{Config, Need, Needs};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// True for a temp file left beside its target by the pre-`persistant` writer.
pub fn is_temp_name(file_name: &str) -> bool {
    file_name.starts_with('.') && file_name.contains(".tmp.")
}

fn scratch_for(dir: &Path) -> PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".to_string());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.with_file_name(format!("{name}.tmp"))
        .join(format!("{}-{n}", std::process::id()))
}

fn other(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Writes `bytes` to `target` atomically. The parent directory is created if missing.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let key = target
        .file_name()
        .ok_or_else(|| other(format!("{} has no file name", target.display())))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(&parent)?;
    let scratch = scratch_for(&parent);
    std::fs::create_dir_all(&scratch)?;
    let result = (|| {
        let needs = Needs::new().with(Need::Write).with(Need::AtomicReplace);
        let store = Store::open(
            Config::Fs {
                root: parent.clone(),
                atomic_write_dir: Some(scratch.clone()),
            },
            needs,
        )
        .map_err(other)?;
        store.replace(&key, bytes.to_vec()).map_err(other)
    })();
    // Our own directory, so removing it cannot touch another writer. Take the now-empty
    // `<dir>.tmp` too when we were the last one in it (fails harmlessly if not).
    let _ = std::fs::remove_dir_all(&scratch);
    if let Some(shared) = scratch.parent() {
        let _ = std::fs::remove_dir(shared);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        write_atomic(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        write_atomic(&p, b"two-longer").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two-longer");
        write_atomic(&p, b"3").unwrap();
        assert_eq!(
            std::fs::read(&p).unwrap(),
            b"3",
            "a shorter write leaves no tail"
        );
    }

    #[test]
    fn creates_missing_parent_directories() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x").join("y").join("z.json");
        write_atomic(&p, b"{}").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn leaves_no_temp_file_behind_on_success() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        write_atomic(&p, b"x").unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.json".to_string()]);
    }

    #[test]
    fn a_failed_write_removes_its_temp_and_keeps_the_old_content() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("keep.json");
        write_atomic(&p, b"original").unwrap();
        // A directory in the target's place makes the final rename fail.
        let blocked = d.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("child"), b"x").unwrap();
        assert!(write_atomic(&blocked, b"nope").is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"original");
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| {
                let n = e.unwrap().file_name().to_string_lossy().into_owned();
                is_temp_name(&n).then_some(n)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn leaves_no_scratch_directory_behind_on_success_or_failure() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("store");
        write_atomic(&dir.join("a.json"), b"x").unwrap();
        // A directory in the target's place makes the final rename fail.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("child")).unwrap();
        assert!(write_atomic(&blocked, b"nope").is_err());
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["store".to_string()], "no store.tmp left");
    }

    #[test]
    fn temp_names_are_recognised_and_real_names_are_not() {
        assert!(is_temp_name(".a.json.tmp.12.0"));
        assert!(!is_temp_name("a.json"));
        assert!(!is_temp_name(".hidden"));
        assert!(!is_temp_name("tmp.json"));
    }
}
