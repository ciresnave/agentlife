// SPDX-License-Identifier: MIT OR Apache-2.0

//! `lane-restart lane-provision <role> [--apply --plan <hash>]` - PM only. Gives an EXISTING lane
//! (one with a recorded launch spec) what a Synapse-reachable lane needs, without creating a repo,
//! a checkout or a spec, and without starting anything.
//! (PM [TASK] fleet-spec-tooling, slice 5; 2026-11-04: six lanes had no Synapse identity or
//! `mcp.json`, so `specs both-channels` refused them and mail to them queued unread.)
//!
//! Steps, each check, act, read back, and `already` when the end state holds (so rerunning converges):
//! 0. validate: a plain role name, not the PM, a spec that reads; an existing `<role>.mcp.json` that
//!    differs from the standard one is kept only when it names a synapse server whose identity
//!    directory exists (the lane already has its own setup), else the run stops, overwriting nothing.
//! 3. the MCP config, written as `lane-new` writes it (the serializer escapes every path).
//! 6. the Synapse identity, through the same provisioning script and read-back `lane-new` uses.
//! 4. the spec's launch arguments brought to the full push-channel form (`specs both-channels`, one
//!    backup, never overwritten). A running session is not touched: it takes this at its next launch.
//!
//! A dry run reads and plans; `--apply` must be given the hash the dry run of the same role printed.

use crate::lane_new::{
    fail, mcp_config, safe_name, step_identity, write_text_atomic, Report, Roots, Status, StepRow,
};
use crate::lane_new_host::Host;
use crate::spec;
use crate::spec_channels;
use crate::spec_synapse::RewriteOutcome;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

/// Fingerprint of what a run would do, from the arguments alone: the role and the roots every path
/// derives from. A typo in the role changes it. Sixteen hex digits.
pub fn plan_hash(roots: &Roots, role: &str) -> String {
    let s = format!(
        "lane-provision plan v1\nrole={role}\nstate_dir={}\nlane_homes={}\npilot_home={}\nchannel_exe={}\n",
        roots.state_dir.display(),
        roots.lane_homes.display(),
        roots.pilot_home.display(),
        roots.channel_exe.display()
    );
    crate::lane_new_remote::sha256_hex(s.as_bytes())[..16].to_string()
}

/// `--apply`: refuses unless `plan` is the [`plan_hash`] of exactly this role. Nothing is read, written
/// or run on a mismatch.
pub fn run_apply(
    roots: &Roots,
    host: Option<&dyn Host>,
    role: &str,
    plan: Option<&str>,
    now: DateTime<Utc>,
) -> Report {
    let refuse = |why: String| Report {
        rows: vec![StepRow {
            n: 0,
            name: "plan",
            status: Status::Fail,
            detail: why,
        }],
        paths: Vec::new(),
        plan: None,
    };
    match plan {
        None => refuse(
            "--apply needs --plan <hash>, printed at the end of a dry run of the same role"
                .to_string(),
        ),
        Some(given) if given == plan_hash(roots, role) => run(roots, host, role, true, now),
        Some(given) => refuse(format!(
            "--plan {given} is not the plan for role {role:?}: dry-run this exact role again and use the hash it prints"
        )),
    }
}

/// Steps 0, 3, 6 and 4 for `role`. Writes and runs only with `apply`. `host` `None` lists step 6 as later.
pub fn run(
    roots: &Roots,
    host: Option<&dyn Host>,
    role: &str,
    apply: bool,
    now: DateTime<Utc>,
) -> Report {
    let mut rows = Vec::new();
    let paths: Vec<PathBuf> = vec![
        roots.state_dir.clone(),
        roots.mcp_path(role),
        roots.spec_path(role),
        roots.lane_home(role),
    ];
    let done = |rows: Vec<StepRow>, apply: bool| Report {
        plan: (!apply && !rows.iter().any(|r| r.status == Status::Fail))
            .then(|| plan_hash(roots, role)),
        rows,
        paths: paths.clone(),
    };

    // --- step 0: validate ----------------------------------------------------------------------
    if !safe_name(role) || role == crate::start::PM_ROLE {
        fail(
            &mut rows,
            0,
            "validate",
            format!("{role:?} is not a role this can provision (a plain name, not the PM)"),
        );
        return done(rows, apply);
    }
    let dir = &roots.state_dir;
    if let Err(e) = spec::read(dir, role) {
        fail(
            &mut rows,
            0,
            "validate",
            format!(
                "no usable launch spec, so not an existing lane (lane-new makes new ones): {e}"
            ),
        );
        return done(rows, apply);
    }
    let want = mcp_config(roots, role);
    let mcp_path = roots.mcp_path(role);
    let existing = std::fs::read_to_string(&mcp_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let mcp_exists = mcp_path.exists();
    let mcp_same = existing.as_ref() == Some(&want);
    // A different config is the lane's own setup only when it already works: it names a synapse
    // server and its identity directory is there. Anything else is a conflict, not overwritten.
    let own_setup = !mcp_same && {
        let (ok, home) = spec_audit_read(dir, role);
        ok && home.is_some_and(|h| std::path::Path::new(&h).join("roles").join(role).is_dir())
    };
    if mcp_exists && !mcp_same && !own_setup {
        fail(
            &mut rows,
            0,
            "validate",
            format!(
                "{} exists and differs from the standard config, and is not a working setup of its own; not overwritten",
                mcp_path.display()
            ),
        );
        return done(rows, apply);
    }
    rows.push(StepRow {
        n: 0,
        name: "validate",
        status: Status::Already,
        detail: format!("{role}: a recorded lane, no conflicting artifact"),
    });

    // --- step 3: MCP config --------------------------------------------------------------------
    if mcp_same || own_setup {
        rows.push(StepRow {
            n: 3,
            name: "mcp config",
            status: Status::Already,
            detail: if mcp_same {
                mcp_path.display().to_string()
            } else {
                format!("{} kept: the lane's own working setup", mcp_path.display())
            },
        });
    } else if !apply {
        rows.push(StepRow {
            n: 3,
            name: "mcp config",
            status: Status::Would,
            detail: format!("write {}", mcp_path.display()),
        });
    } else {
        let text = serde_json::to_string_pretty(&want).unwrap_or_default() + "\n";
        let back = write_text_atomic(&mcp_path, &text).and_then(|()| {
            let t = std::fs::read_to_string(&mcp_path).map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
            if v == want {
                Ok(())
            } else {
                Err("the file read back differs from what was written".to_string())
            }
        });
        match back {
            Ok(()) => rows.push(StepRow {
                n: 3,
                name: "mcp config",
                status: Status::Ok,
                detail: format!("{} read back equal", mcp_path.display()),
            }),
            Err(e) => {
                fail(&mut rows, 3, "mcp config", e);
                return done(rows, apply);
            }
        }
    }

    // --- step 6: the Synapse identity ------------------------------------------------------------
    if own_setup {
        rows.push(StepRow {
            n: 6,
            name: "synapse identity",
            status: Status::Already,
            detail: "the lane's own config names a home that holds the role's identity".to_string(),
        });
    } else {
        match host {
            None => rows.push(StepRow {
                n: 6,
                name: "synapse identity",
                status: Status::Later,
                detail: "no host in this build".to_string(),
            }),
            Some(h) => match step_identity(h, roots, role, apply) {
                Ok((status, detail)) => rows.push(StepRow {
                    n: 6,
                    name: "synapse identity",
                    status,
                    detail,
                }),
                Err(e) => {
                    fail(&mut rows, 6, "synapse identity", e);
                    return done(rows, apply);
                }
            },
        }
    }

    // --- step 4: launch arguments to the full push-channel form ---------------------------------
    // A dry run cannot ask `rewrite_roles` while the config it needs is still to be written.
    if !apply && !mcp_path.exists() {
        rows.push(StepRow {
            n: 4,
            name: "launch channels",
            status: Status::Would,
            detail:
                "bring the spec to --mcp-config + server:claude-peers server:synapse, one backup"
                    .to_string(),
        });
        return done(rows, apply);
    }
    let r = spec_channels::rewrite_roles(dir, &[role.to_string()], !apply, now)
        .pop()
        .map(|r| r.outcome);
    match r {
        Some(RewriteOutcome::Rewrote(a)) => rows.push(StepRow {
            n: 4,
            name: "launch channels",
            status: Status::Ok,
            detail: format!("launch_args now {}", a.join(" ")),
        }),
        Some(RewriteOutcome::WouldRewrite(a)) => rows.push(StepRow {
            n: 4,
            name: "launch channels",
            status: Status::Would,
            detail: format!("launch_args would be {}", a.join(" ")),
        }),
        Some(RewriteOutcome::Unchanged) => rows.push(StepRow {
            n: 4,
            name: "launch channels",
            status: Status::Already,
            detail: "already the full form".to_string(),
        }),
        Some(RewriteOutcome::Refused(why)) => fail(&mut rows, 4, "launch channels", why),
        None => fail(&mut rows, 4, "launch channels", "no result".to_string()),
    }
    done(rows, apply)
}

/// `(config names a synapse server, the identity home it names)`.
fn spec_audit_read(dir: &std::path::Path, role: &str) -> (bool, Option<String>) {
    let (mcp, home) = crate::spec_audit::read_mcp(dir, role);
    (mcp.is_ok(), home)
}

#[cfg(test)]
mod tests;
