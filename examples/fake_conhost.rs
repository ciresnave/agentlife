// SPDX-License-Identifier: MIT OR Apache-2.0
//! A stand-in for `conhost.exe <program> <args...>`, for the launcher's tests.
//!
//! The real `conhost.exe` opens a console window and runs the command line it is given in it, staying
//! alive while that command runs. On 2026-10-07 it proved unreliable as a test subject: on the GitHub
//! Windows runner it exited after ~120 ms without running the command on every attempt, and on a
//! developer machine it did so about half the time. The launcher's own behaviour (which program it
//! starts, with what arguments, working directory and environment, and whether it believes the start)
//! can be tested without it: this stand-in runs the command as its child, inherits the working
//! directory and environment it was started with, waits, and exits with the child's status.

use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((program, rest)) = args.split_first() else {
        eprintln!("fake_conhost: expected <program> <args...>");
        std::process::exit(2);
    };
    let status = Command::new(program)
        .args(rest)
        .status()
        .expect("start the command");
    std::process::exit(status.code().unwrap_or(1));
}
