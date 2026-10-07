// SPDX-License-Identifier: MIT OR Apache-2.0
//! A stand-in for `lane-restart host`, for the launcher's tests.
//!
//! `fake_host host --role <role> -- <program> <args...>` is the shape the launcher builds
//! (`DESIGN.md` §6). The stand-in:
//!
//! 1. writes `fake-host-marker.json` into its working directory (the one thing `wt.exe` is known to
//!    forward): the role, the argv it was given, and whether `AGENTLIFE_AGENT_ID` reached it;
//! 2. if `FAKE_AGENTLIFE` is set, runs `<program> <args...>` as its child (the stand-in `claude`, which
//!    registers itself through the real hook) and exits with that child's status; otherwise it exits at
//!    once. The second case is the real-`wt.exe` run, where no environment is forwarded and the stand-in
//!    must not touch any registry.

use std::process::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (role, child_argv) = match args.as_slice() {
        [host, flag, role, sep, rest @ ..] if host == "host" && flag == "--role" && sep == "--" => {
            (role.clone(), rest.to_vec())
        }
        _ => {
            eprintln!("fake_host: expected `host --role <role> -- <argv...>`, got {args:?}");
            std::process::exit(2);
        }
    };
    let marker = serde_json::json!({
        "role": role,
        "argv": child_argv,
        "agent_id": std::env::var("AGENTLIFE_AGENT_ID").ok(),
        "pid": std::process::id(),
    });
    let dir = std::env::current_dir().expect("cwd");
    let _ = std::fs::write(dir.join("fake-host-marker.json"), marker.to_string());

    if std::env::var_os("FAKE_AGENTLIFE").is_none() {
        return;
    }
    let (program, rest) = child_argv.split_first().expect("a program after --");
    let status = Command::new(program)
        .args(rest)
        .status()
        .expect("start the stand-in claude");
    std::process::exit(status.code().unwrap_or(1));
}
