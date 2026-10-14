// SPDX-License-Identifier: MIT OR Apache-2.0

//! The disk floor: nothing is launched while the volume holding the lane state has less than
//! [`FLOOR_BYTES`] free.
//!
//! Incident 2026-10-14 04:45Z: `C:` reached 4 GB free and builds failed in six lanes at once. A
//! launch starts a build-capable session, so it is the one moment this tool can refuse before the
//! disk is the thing that is failing.
//!
//! The decision is a pure function over numbers ([`check`]); the volume is chosen by a pure function
//! over a list of mounts ([`volume_for`]); only [`probe`] reads the machine.

use std::fmt;
use std::path::Path;

/// 20 GiB. Windows reports GiB as "GB", so this is the "20 GB" the PM named.
pub const FLOOR_BYTES: u64 = 20 * (1 << 30);

/// Debug builds only: overrides [`FLOOR_BYTES`], so tests do not depend on how full the box is. A
/// release build ignores it, because a lane that could set it to `0` would have no floor.
pub const FLOOR_ENV: &str = "LANE_DISK_FLOOR_BYTES";

/// Debug builds only: the path whose volume is measured instead of the lane state directory. A test
/// on a machine where `C:/Projects/.lane-state` is on no mount (Linux CI) points it at a temp dir,
/// so the refusal is exercised there too. A release build ignores it.
pub const PROBE_PATH_ENV: &str = "LANE_DISK_PROBE_PATH";

/// The path to measure: `default`, or in a debug build `$LANE_DISK_PROBE_PATH` when it is set.
fn probe_path(default: &Path) -> std::path::PathBuf {
    if cfg!(debug_assertions) {
        if let Some(p) = std::env::var_os(PROBE_PATH_ENV).filter(|p| !p.is_empty()) {
            return p.into();
        }
    }
    default.to_path_buf()
}

/// The floor in force: [`FLOOR_BYTES`], or in a debug build `$LANE_DISK_FLOOR_BYTES` when it parses.
pub fn floor_bytes() -> u64 {
    if cfg!(debug_assertions) {
        if let Some(n) = std::env::var(FLOOR_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            return n;
        }
    }
    FLOOR_BYTES
}

/// What was measured, on which volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Free {
    /// The mount point the path resolved to, e.g. `C:\`.
    pub mount: String,
    pub bytes: u64,
}

/// Free space was below the floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub mount: String,
    pub free: u64,
    pub floor: u64,
}

fn gib(b: u64) -> String {
    format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "disk floor: {} has {} free, below the {} floor; nothing was launched \
             (free space on {} and retry)",
            self.mount,
            gib(self.free),
            gib(self.floor),
            self.mount
        )
    }
}

/// Is `free` enough? Exactly the floor passes; one byte less does not.
pub fn check(free: &Free, floor: u64) -> Result<(), Refusal> {
    if free.bytes < floor {
        Err(Refusal {
            mount: free.mount.clone(),
            free: free.bytes,
            floor,
        })
    } else {
        Ok(())
    }
}

fn norm(s: &str) -> String {
    let t = s.replace('\\', "/").to_lowercase();
    let t = t.strip_prefix("//?/").unwrap_or(&t);
    t.trim_end_matches('/').to_string()
}

/// The mount `path` lives on: the longest mount that is a path-prefix of it, compared
/// case-insensitively with `\` and `/` alike. `None` when no mount contains it.
pub fn volume_for<'a>(path: &str, mounts: &'a [(String, u64)]) -> Option<&'a (String, u64)> {
    let p = norm(path);
    mounts
        .iter()
        .filter(|(m, _)| {
            let m = norm(m);
            p == m || p.starts_with(&format!("{m}/"))
        })
        .max_by_key(|(m, _)| norm(m).len())
}

/// Free bytes on the volume holding `path`, from the operating system. `Err` says why it could not
/// be read; that is not the same as "too little".
pub fn probe(path: &Path) -> Result<Free, String> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mounts: Vec<(String, u64)> = disks
        .list()
        .iter()
        .map(|d| {
            (
                d.mount_point().to_string_lossy().into_owned(),
                d.available_space(),
            )
        })
        .collect();
    let p = path.to_string_lossy();
    volume_for(&p, &mounts)
        .map(|(m, bytes)| Free {
            mount: m.clone(),
            bytes: *bytes,
        })
        .ok_or_else(|| {
            format!(
                "no mounted volume contains {p} ({} volumes listed)",
                mounts.len()
            )
        })
}

/// The gate every launch path calls first: `Ok(Free)` when there is enough, `Err(refusal)` when
/// there is not, and `Ok(Unknown(why))` when free space could not be read. **Unreadable is not
/// refused**: a broken probe must not stop every lane from restarting; the caller prints `why`.
pub fn guard(path: &Path) -> Result<GuardOk, Refusal> {
    match probe(&probe_path(path)) {
        Ok(free) => check(&free, floor_bytes()).map(|()| GuardOk::Free(free)),
        Err(why) => Ok(GuardOk::Unknown(why)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOk {
    Free(Free),
    /// The probe failed; the launch goes ahead and this says why it was not checked.
    Unknown(String),
}

/// One line for `status`: `C: free: 123.4 GB (floor 20.0 GB)` or why it is unknown.
pub fn status_line(path: &Path) -> String {
    match probe(&probe_path(path)) {
        Ok(f) => format!(
            "{} free: {} (floor {}){}",
            f.mount,
            gib(f.bytes),
            gib(floor_bytes()),
            if f.bytes < floor_bytes() {
                " BELOW FLOOR: launches are refused"
            } else {
                ""
            }
        ),
        Err(why) => format!("disk free: unknown ({why})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mounts() -> Vec<(String, u64)> {
        vec![
            ("C:\\".into(), 4),
            ("D:\\".into(), 900),
            ("C:\\mnt\\big".into(), 777),
        ]
    }

    #[test]
    fn the_floor_is_exact() {
        let f = |bytes| Free {
            mount: "C:\\".into(),
            bytes,
        };
        assert!(check(&f(FLOOR_BYTES), FLOOR_BYTES).is_ok());
        let r = check(&f(FLOOR_BYTES - 1), FLOOR_BYTES).unwrap_err();
        assert_eq!(r.free, FLOOR_BYTES - 1);
        assert!(r.to_string().contains("C:\\"), "{r}");
        assert!(r.to_string().contains("20.0 GB"), "{r}");
    }

    #[test]
    fn the_volume_is_the_longest_containing_mount() {
        let m = mounts();
        // Both slash forms, any case: the state dir is written `C:/Projects/.lane-state`.
        assert_eq!(volume_for("C:/Projects/.lane-state", &m).unwrap().1, 4);
        assert_eq!(volume_for("c:\\projects", &m).unwrap().1, 4);
        assert_eq!(volume_for("D:/x", &m).unwrap().1, 900);
        // A nested mount wins over its parent (positive control: the line above resolves to C:\).
        assert_eq!(volume_for("C:/mnt/big/x", &m).unwrap().1, 777);
        // `C:\mnt\bigger` is not under `C:\mnt\big`.
        assert_eq!(volume_for("C:/mnt/bigger", &m).unwrap().1, 4);
        // The verbatim prefix `canonicalize` produces on Windows.
        assert_eq!(volume_for("\\\\?\\C:\\Projects", &m).unwrap().1, 4);
        // No mount contains it.
        assert!(volume_for("E:/x", &m).is_none());
    }

    #[test]
    fn a_unix_root_mount_contains_everything_absolute() {
        let m = vec![("/".to_string(), 5), ("/home".to_string(), 50)];
        assert_eq!(volume_for("/var/x", &m).unwrap().1, 5);
        assert_eq!(volume_for("/home/u", &m).unwrap().1, 50);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn the_env_override_is_honoured_in_a_debug_build_only() {
        // Serialised by being the only test that touches the variable.
        std::env::set_var(FLOOR_ENV, "0");
        assert_eq!(floor_bytes(), 0);
        std::env::set_var(FLOOR_ENV, "not a number");
        assert_eq!(floor_bytes(), FLOOR_BYTES);
        std::env::remove_var(FLOOR_ENV);
        assert_eq!(floor_bytes(), FLOOR_BYTES);
    }
}
