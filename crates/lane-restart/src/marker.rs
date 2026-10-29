// SPDX-License-Identifier: MIT OR Apache-2.0
//! Integrity of the soft-stop marker (docs/M5-WAKE-DESIGN.md slice 4, PM ruling 2026-10-28).
//!
//! `stop_kind: soft` in `<role>.launch.json` is the implied grant of board 216: it is what lets the
//! launcher start a stopped lane. As a plain field, any process that can write the lane-state
//! directory could set it. [`sign`] puts an HMAC-SHA256 over the whole spec (every field but the MAC)
//! beside it, keyed from a random key kept in `<dir>/spec-mac.key`; [`verify`] recomputes it.
//! `spec::write_atomic` signs every spec it writes that records a soft stop, and `spec::read` treats
//! a soft stop whose MAC is missing or wrong as *unclassified*, which every launcher refuses
//! (`start::launcher_may_start`): unverified means hard.
//!
//! The key is DPAPI-protected on Windows (the `src/consent/real.rs` pattern) and a `0600` file
//! elsewhere. **Stated limit, as in `DESIGN-REVISION-2` 7.4:** `stop --self` runs as the lane, so the
//! lane's own user can reach the same key and call the signer. This stops an edit of the file and a
//! casual forger, not a same-user process that deliberately signs. The bound if it is forged anyway
//! is the one `LAUNCH-ON-MESSAGE-THREAT.md` accepts: start a lane that has a spec, on Sonnet in mode
//! `auto`, under the hourly and daily caps.
//!
//! A spec's serialized form is the MAC input, so a field added to `SpecFile` later must be
//! `skip_serializing_if = "Option::is_none"` or every earlier signature stops verifying.

use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::spec::SpecFile;

type HmacSha256 = Hmac<Sha256>;

const KEY_FILE: &str = "spec-mac.key";
const KEY_LEN: usize = 32;

pub fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

/// The bytes the MAC covers: the spec as serialized with no MAC.
fn message(spec: &SpecFile) -> Option<Vec<u8>> {
    let mut bare = spec.clone();
    bare.stop_mac = None;
    serde_json::to_vec(&bare).ok()
}

fn mac_hex(key: &[u8], msg: &[u8]) -> Option<String> {
    let mut m = HmacSha256::new_from_slice(key).ok()?;
    m.update(msg);
    Some(hex(&m.finalize().into_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// The MAC for `spec`, creating the key if there is none. `None` when no key can be had (the spec is
/// then written without a MAC and its soft stop reads as unclassified).
pub fn sign(dir: &Path, spec: &SpecFile) -> Option<String> {
    let key = load_key(dir, true)?;
    mac_hex(&key, &message(spec)?)
}

/// True only for a spec carrying a MAC that matches its content under the key already on disk.
/// Never creates a key: no key means nothing was ever signed here.
pub fn verify(dir: &Path, spec: &SpecFile) -> bool {
    let Some(given) = spec.stop_mac.as_deref().and_then(unhex) else {
        return false;
    };
    let Some(key) = load_key(dir, false) else {
        return false;
    };
    let Some(msg) = message(spec) else {
        return false;
    };
    let Ok(mut m) = HmacSha256::new_from_slice(&key) else {
        return false;
    };
    m.update(&msg);
    m.verify_slice(&given).is_ok()
}

fn load_key(dir: &Path, create: bool) -> Option<Vec<u8>> {
    let path = key_path(dir);
    match std::fs::read(&path) {
        Ok(blob) => return unprotect(&blob).filter(|k| k.len() == KEY_LEN),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    if !create {
        return None;
    }
    let mut key = [0u8; KEY_LEN];
    getrandom::fill(&mut key).ok()?;
    let blob = protect(&key)?;
    std::fs::create_dir_all(dir).ok()?;
    match create_new_private(&path, &blob) {
        Ok(()) => Some(key.to_vec()),
        // Another process created it first: use theirs, never overwrite.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let blob = std::fs::read(&path).ok()?;
            unprotect(&blob).filter(|k| k.len() == KEY_LEN)
        }
        Err(_) => None,
    }
}

fn create_new_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(windows)]
fn protect(key: &[u8]) -> Option<Vec<u8>> {
    dpapi::protect(key)
}
#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    dpapi::unprotect(blob)
}
#[cfg(not(windows))]
fn protect(key: &[u8]) -> Option<Vec<u8>> {
    Some(key.to_vec())
}
#[cfg(not(windows))]
fn unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    Some(blob.to_vec())
}

/// Windows DPAPI, bound to the current user (`CRYPTPROTECT_UI_FORBIDDEN`, no machine scope).
#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    const ENTROPY: &[u8] = b"agentlife lane-restart spec-mac v1";

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: b.len() as u32,
            pbData: b.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: on success DPAPI hands back `cbData` bytes at `pbData`, allocated for us to free.
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the blobs point at live slices for the call; `out` is filled by the API.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        (ok != 0).then(|| take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        (ok != 0).then(|| take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{SpecState, StopKind};

    fn soft(_dir: &Path) -> SpecFile {
        let role = lane_state::role::Role::new("alpha").unwrap();
        SpecFile {
            schema: crate::spec::SCHEMA,
            role: role.clone(),
            name: role,
            cwd: "C:/Projects/alpha".into(),
            model: "sonnet".into(),
            permission_mode: None,
            remote_control: false,
            launch_args: vec![],
            handoff_path: None,
            handoff_mtime: None,
            state: SpecState::Suspended,
            written_at: chrono::Utc::now(),
            written_by: "test".into(),
            stopped_session_id: None,
            stopped_pid_start_secs: None,
            stop_reason: None,
            stop_kind: Some(StopKind::Soft),
            stop_mac: None,
            tool_version: "test".into(),
            launch: None,
            wsl: None,
        }
    }

    #[test]
    fn a_signed_spec_verifies_and_any_edited_field_does_not() {
        let d = tempfile::tempdir().unwrap();
        let mut s = soft(d.path());
        s.stop_mac = sign(d.path(), &s);
        assert!(s.stop_mac.is_some());
        assert!(verify(d.path(), &s), "control: the untouched spec verifies");
        let mut edited = s.clone();
        edited.cwd = "C:/elsewhere".into();
        assert!(!verify(d.path(), &edited), "cwd");
        let mut edited = s.clone();
        edited.launch_args.push("--x".into());
        assert!(!verify(d.path(), &edited), "launch_args");
        let mut edited = s.clone();
        edited.stop_kind = Some(StopKind::Hard);
        assert!(!verify(d.path(), &edited), "stop_kind");
        let mut edited = s;
        edited.stop_mac = Some("00".repeat(32));
        assert!(!verify(d.path(), &edited), "a made-up MAC");
    }

    #[test]
    fn no_mac_no_key_or_another_key_is_unverified_and_verify_never_makes_a_key() {
        let d = tempfile::tempdir().unwrap();
        let s = soft(d.path());
        assert!(!verify(d.path(), &s), "no MAC");
        assert!(!key_path(d.path()).exists(), "verify created a key");
        let mut signed = s.clone();
        signed.stop_mac = sign(d.path(), &s);
        assert!(verify(d.path(), &signed));
        let other = tempfile::tempdir().unwrap();
        assert!(!verify(other.path(), &signed), "a directory with no key");
        sign(other.path(), &s);
        assert!(!verify(other.path(), &signed), "a different key");
    }

    #[test]
    fn the_key_is_created_once_and_a_damaged_key_verifies_nothing() {
        let d = tempfile::tempdir().unwrap();
        let s = soft(d.path());
        let a = sign(d.path(), &s);
        let b = sign(d.path(), &s);
        assert_eq!(a, b, "the second signing reused the key");
        std::fs::write(key_path(d.path()), b"short").unwrap();
        let mut signed = s;
        signed.stop_mac = a;
        assert!(!verify(d.path(), &signed));
        assert!(
            sign(d.path(), &signed).is_none(),
            "a bad key is not replaced"
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_key_on_disk_is_not_the_key() {
        let d = tempfile::tempdir().unwrap();
        let s = soft(d.path());
        sign(d.path(), &s).unwrap();
        let blob = std::fs::read(key_path(d.path())).unwrap();
        let key = load_key(d.path(), false).unwrap();
        assert_ne!(blob, key, "DPAPI wrapped it");
        assert!(blob.len() > KEY_LEN);
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(unhex(&hex(&[0, 1, 254, 255])), Some(vec![0, 1, 254, 255]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
