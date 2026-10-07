// SPDX-License-Identifier: MIT OR Apache-2.0
//! Write a file so a reader never sees half of it: write a sibling temp file, flush it to disk,
//! then rename it over the target (the temp + rename pattern `lane-restart`'s `write_atomic`
//! uses; DESIGN-REVISION-1 §4.3 item 1).
//!
//! Temp files are named `.<name>.tmp.<pid>.<n>` so a crash can leave one behind, and
//! [`is_temp_name`] lets a lister skip it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path_for(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = format!(".{name}.tmp.{}.{n}", std::process::id());
    target.with_file_name(tmp)
}

/// True for a name produced by [`write_atomic`]'s temp files.
pub fn is_temp_name(file_name: &str) -> bool {
    file_name.starts_with('.') && file_name.contains(".tmp.")
}

/// Writes `bytes` to `target` atomically. The parent directory is created if missing.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_path_for(target);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
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
    fn temp_names_are_recognised_and_real_names_are_not() {
        assert!(is_temp_name(".a.json.tmp.12.0"));
        assert!(!is_temp_name("a.json"));
        assert!(!is_temp_name(".hidden"));
        assert!(!is_temp_name("tmp.json"));
    }
}
