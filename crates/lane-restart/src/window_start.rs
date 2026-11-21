// SPDX-License-Identifier: MIT OR Apache-2.0

//! Starting a lane's `wt.exe` window MINIMIZED (CireSnave 2026-11-19: *"I don't need to see every lane
//! start. Is it possible to have their windows start minimized?"*).
//!
//! Measured 2026-11-20 on scratch `wt.exe -w new --title lane-X --suppressApplicationTitle cmd /c timeout`
//! launches, N=10 treatment and N=10 untouched control, interleaved: finding the window by class
//! `CASCADIA_HOSTING_WINDOW_CLASS` and its exact title and calling `ShowWindow(SW_MINIMIZE)` every 250 ms
//! until `IsIconic` left it iconic at 1, 2 and 4 s afterwards in 10/10 launches (control 0/10), about
//! 0.9-1.3 s after the launch. What does NOT work, same session: the start state of the process that runs
//! `wt.exe` (`Start-Process -WindowStyle Minimized` 1/3 then 0/2, `CreateProcessW` with `wShowWindow` 7 and
//! 2, `start /min`: 0/1 each, and Windows Terminal ignores it when it joins a running instance), a single
//! `SW_SHOWMINNOACTIVE` (0/5), and a single `SW_MINIMIZE` at first sight, ~370 ms (0/10: the terminal's
//! own first show overrides it). So the window is seen for a moment; it is then minimized and left there.
//!
//! `LANE_START_WINDOW=normal` turns it off (the title and the minimize), any other value or none is on.

use std::ffi::OsStr;
use std::time::Duration;

/// The environment variable that undoes the minimized start.
pub const WINDOW_ENV: &str = "LANE_START_WINDOW";

/// How long a launch looks for its window and waits for it to stay minimized.
pub const MINIMIZE_LIMIT: Duration = Duration::from_secs(5);

/// How a lane's window opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStart {
    Minimized,
    Normal,
}

impl WindowStart {
    /// From the value of [`WINDOW_ENV`]: `normal` (any case) keeps the window normal, everything else, and
    /// unset, is minimized.
    pub fn from_env_value(v: Option<&OsStr>) -> Self {
        match v.map(|v| v.to_string_lossy().trim().to_ascii_lowercase()) {
            Some(s) if s == "normal" => WindowStart::Normal,
            _ => WindowStart::Minimized,
        }
    }

    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var_os(WINDOW_ENV).as_deref())
    }
}

/// The fixed title of a lane's window, which is how the window is found again: `lane-<role>`.
pub fn window_title(role: &str) -> String {
    format!("lane-{role}")
}

/// How a minimize attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MinimizeOutcome {
    /// Every window with the title was iconic after `calls` calls, `ms` after the first look.
    Minimized { calls: u32, ms: u64 },
    /// No window with the title appeared within the limit.
    NotFound,
    /// Windows were found but did not stay iconic within the limit.
    StillNormal { calls: u32 },
}

impl MinimizeOutcome {
    /// One line for `spawn.log`.
    pub fn describe(&self) -> String {
        match self {
            MinimizeOutcome::Minimized { calls, ms } => {
                format!("window minimized after {ms} ms ({calls} calls)")
            }
            MinimizeOutcome::NotFound => "window not found to minimize".to_string(),
            MinimizeOutcome::StillNormal { calls } => {
                format!("window still not minimized after {calls} calls")
            }
        }
    }
}

/// Looks for the windows of a launch and minimizes them until they stay minimized or `limit` passes.
/// `find` lists the handles of the windows with the lane's title, `iconic` says whether one is minimized,
/// `minimize` asks for it, `sleep` waits (all injected: this is the logic, not the OS calls). Polls every
/// 20 ms until a window shows, then asks every 250 ms: a request at first sight is overridden by the
/// terminal's own first show.
pub fn minimize_until(
    find: &mut dyn FnMut() -> Vec<isize>,
    iconic: &mut dyn FnMut(isize) -> bool,
    minimize: &mut dyn FnMut(isize),
    sleep: &mut dyn FnMut(Duration),
    limit: Duration,
) -> MinimizeOutcome {
    let look = Duration::from_millis(20);
    let ask = Duration::from_millis(250);
    let mut elapsed = Duration::ZERO;
    let mut calls = 0u32;
    let mut seen = false;
    while elapsed <= limit {
        let windows = find();
        if windows.is_empty() {
            sleep(look);
            elapsed += look;
            continue;
        }
        seen = true;
        let mut all = true;
        for &h in &windows {
            if !iconic(h) {
                all = false;
                minimize(h);
                calls += 1;
            }
        }
        if all {
            return MinimizeOutcome::Minimized {
                calls,
                ms: elapsed.as_millis() as u64,
            };
        }
        sleep(ask);
        elapsed += ask;
    }
    if seen {
        MinimizeOutcome::StillNormal { calls }
    } else {
        MinimizeOutcome::NotFound
    }
}

/// Minimizes the window titled `title` for real. Windows only; off Windows there is no `wt.exe` window.
#[cfg(windows)]
pub fn minimize_window(title: &str, limit: Duration) -> MinimizeOutcome {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextW, IsIconic, ShowWindow, SW_MINIMIZE,
    };

    struct Search {
        title: Vec<u16>,
        found: Vec<isize>,
    }
    unsafe extern "system" fn visit(h: HWND, l: LPARAM) -> i32 {
        // SAFETY: `l` is the address of the `Search` that `EnumWindows` was handed below, alive for the call.
        let s = unsafe { &mut *(l as *mut Search) };
        let mut class = [0u16; 64];
        // SAFETY: the buffer is valid for its stated length.
        let n = unsafe { GetClassNameW(h, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if class == "CASCADIA_HOSTING_WINDOW_CLASS" {
            let mut text = [0u16; 256];
            // SAFETY: as above.
            let n = unsafe { GetWindowTextW(h, text.as_mut_ptr(), text.len() as i32) };
            if text[..n.max(0) as usize] == s.title[..] {
                s.found.push(h as isize);
            }
        }
        1
    }

    let wanted: Vec<u16> = title.encode_utf16().collect();
    minimize_until(
        &mut || {
            let mut s = Search {
                title: wanted.clone(),
                found: Vec::new(),
            };
            // SAFETY: `visit` only reads the window it is given and writes through `s`, which outlives the call.
            unsafe { EnumWindows(Some(visit), &mut s as *mut Search as LPARAM) };
            s.found
        },
        // SAFETY (both): a stale handle makes these return false, nothing worse.
        &mut |h| unsafe { IsIconic(h as HWND) } != 0,
        &mut |h| {
            unsafe { ShowWindow(h as HWND, SW_MINIMIZE) };
        },
        &mut std::thread::sleep,
        limit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn only_the_word_normal_keeps_the_window_normal() {
        let v = |s: &str| WindowStart::from_env_value(Some(OsStr::new(s)));
        assert_eq!(v("normal"), WindowStart::Normal);
        assert_eq!(v(" Normal "), WindowStart::Normal);
        assert_eq!(v("minimized"), WindowStart::Minimized);
        assert_eq!(v(""), WindowStart::Minimized);
        assert_eq!(WindowStart::from_env_value(None), WindowStart::Minimized);
    }

    #[test]
    fn a_lanes_window_title_names_the_lane() {
        assert_eq!(window_title("kiss"), "lane-kiss");
    }

    /// A window that shows up on the 3rd look, ignores the first request (the terminal's own first show)
    /// and goes iconic on the 2nd.
    #[test]
    fn it_waits_for_the_window_and_asks_again_until_it_stays_minimized() {
        let looks = Cell::new(0u32);
        let asked = Cell::new(0u32);
        let slept = RefCell::new(Duration::ZERO);
        let out = minimize_until(
            &mut || {
                looks.set(looks.get() + 1);
                if looks.get() >= 3 {
                    vec![7]
                } else {
                    vec![]
                }
            },
            &mut |_| asked.get() >= 2,
            &mut |_| asked.set(asked.get() + 1),
            &mut |d| *slept.borrow_mut() += d,
            Duration::from_secs(5),
        );
        assert_eq!(asked.get(), 2, "two requests: the first is overridden");
        assert!(
            matches!(out, MinimizeOutcome::Minimized { calls: 2, .. }),
            "{out:?}"
        );
        // two 20 ms looks before the window shows, then a 250 ms wait after each of the two requests
        assert_eq!(*slept.borrow(), Duration::from_millis(20 * 2 + 250 * 2));
    }

    #[test]
    fn a_window_that_never_appears_is_not_found_and_one_that_never_minimizes_is_said_so() {
        let slept = RefCell::new(Duration::ZERO);
        let none = minimize_until(
            &mut Vec::new,
            &mut |_| true,
            &mut |_| {},
            &mut |d| *slept.borrow_mut() += d,
            Duration::from_secs(1),
        );
        assert_eq!(none, MinimizeOutcome::NotFound);
        assert!(*slept.borrow() >= Duration::from_secs(1));
        // control: a window that shows but ignores every request is not reported as minimized
        let stuck = minimize_until(
            &mut || vec![1],
            &mut |_| false,
            &mut |_| {},
            &mut |_| {},
            Duration::from_secs(1),
        );
        assert!(
            matches!(stuck, MinimizeOutcome::StillNormal { calls } if calls > 0),
            "{stuck:?}"
        );
    }

    #[test]
    fn every_window_with_the_title_is_minimized_not_just_one() {
        // a restart can leave the old window beside the new one
        let state = RefCell::new(vec![(1isize, false), (2isize, false)]);
        let out = minimize_until(
            &mut || state.borrow().iter().map(|(h, _)| *h).collect(),
            &mut |h| state.borrow().iter().any(|(x, i)| *x == h && *i),
            &mut |h| {
                for (x, i) in state.borrow_mut().iter_mut() {
                    if *x == h {
                        *i = true;
                    }
                }
            },
            &mut |_| {},
            Duration::from_secs(5),
        );
        assert!(
            matches!(out, MinimizeOutcome::Minimized { calls: 2, .. }),
            "{out:?}"
        );
        assert!(state.borrow().iter().all(|(_, i)| *i));
    }

    #[test]
    fn the_log_line_says_what_happened() {
        assert_eq!(
            MinimizeOutcome::Minimized { calls: 2, ms: 910 }.describe(),
            "window minimized after 910 ms (2 calls)"
        );
        assert_eq!(
            MinimizeOutcome::NotFound.describe(),
            "window not found to minimize"
        );
    }
}
