# Roster file schema (draft, schema 1)

Follows `DESIGN.md` §1.3. **Draft for review; nothing implements it yet.**
Location: `C:/Projects/.agentlife/roster.json`, outside every repo. Written by `agentlife roster …` or by
hand; never by a lane's hook.

## Fields

Top level:

| field | type | required | meaning |
|---|---|---|---|
| `schema` | integer | yes | `1`. An unknown value refuses the file. |
| `defaults` | object | no | batch and timing defaults (below). |
| `lanes` | array | yes | one entry per lane. |

`defaults` (each optional, shown with the proposed default): `batch_size` 3, `batch_delay_secs` 45,
`liveness_timeout_secs` 120, `stop_after_failed_batches` 2, `handoff_stale_after_secs` 600,
`network_wait_secs` 120.

Per lane:

| field | type | req | rule |
|---|---|---|---|
| `role` | string | yes | `[A-Za-z0-9_-]{1,64}`, unique, case-insensitive-unique (the restart log has both `Unpopped` and `unpopped` today). **The explicit id; never derived from a cwd.** |
| `enabled` | bool | no (true) | `false` = never started by `restore`. |
| `order` | integer ≥ 0 | yes | 0 starts alone first. Lower first; equal values start in one batch pool. |
| `cwd` | string | yes | launch directory, must exist, under `C:/Projects`. |
| `name` | string \| null | yes | the `--name` value; same charset; unique per `(cwd, name)`. `null` means "use `role`", but then `restore` cannot recognise an already-running copy by name (DESIGN §2.4 rule 2). |
| `model` | string \| null | yes | passed verbatim; `null` omits `--model`. |
| `mode` | string \| null | yes | approved permission-mode ceiling. `null` omits `--permission-mode` (Claude Code default). Only `pm` may be `bypassPermissions`. |
| `remote_control` | bool | no (false) | adds `--remote-control`. |
| `channels` | string[] | no ([]) | each becomes part of `--dangerously-load-development-channels`. v0.1: only `server:claude-peers`. |
| `extra_args` | string[] | no ([]) | allowlisted flags only (`--add-dir`, `--mcp-config`, `--settings`). v0.1: must be empty. |
| `handoff` | string \| null | no | path to the lane's HANDOFF; `null` = none exists (report says so). |
| `env` | object | no | `LANE_ROLE` only in v1; any other key refuses the file. |
| `notes` | string | no | free text for people; never read by the tool. |

An unknown key anywhere refuses the file (a typo such as `"enabeld"` must never mean "enabled").

## Example from the 15 live lanes

Built from the live command lines and `.lane-state/*.json` read 2026-10-03 ~15:36Z. It is a **starting
point to correct**, not a decision: `order` and `enabled` are my guesses, and the choices marked
`CHECK` are things I could not read from the process table.

```json
{
  "schema": 1,
  "lanes": [
    { "role": "pm", "enabled": true, "order": 0, "cwd": "C:/Projects", "name": "PM",
      "model": "sonnet", "mode": "bypassPermissions", "remote_control": false,
      "channels": ["server:claude-peers"], "handoff": "C:/Projects/PM-HANDOFF.md",
      "env": { "LANE_ROLE": "pm" } },

    { "role": "overmind", "order": 1, "cwd": "C:/Projects/OverMind", "name": "overmind",
      "model": "claude-opus-5-5", "mode": "auto", "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/OverMind/HANDOFF.md" },
    { "role": "synapse", "order": 1, "cwd": "C:/Projects/synapse", "name": "synapse",
      "model": "claude-opus-5-5", "mode": "auto", "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/synapse/HANDOFF.md" },

    { "role": "unpopped", "order": 2, "cwd": "C:/Projects/Unpopped", "name": "unpopped",
      "model": "claude-opus-5-5", "mode": "auto", "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/Unpopped/HANDOFF.md" },
    { "role": "baracuda", "order": 2, "cwd": "C:/Projects/baracuda", "name": "baracuda",
      "model": "claude-sonnet-5", "mode": null, "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/baracuda/HANDOFF.md" },
    { "role": "fuel", "order": 2, "cwd": "C:/Projects/fuel", "name": "fuel",
      "model": "claude-sonnet-5", "mode": null, "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/fuel/HANDOFF.md" },
    { "role": "mlmf", "order": 2, "cwd": "C:/Projects/mlmf", "name": "mlmf",
      "model": "claude-sonnet-5", "mode": null, "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/mlmf/HANDOFF.md" },
    { "role": "lightbulb", "order": 2, "cwd": "C:/Projects/lightbulb", "name": "lightbulb",
      "model": "claude-sonnet-5", "mode": "auto", "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/lightbulb/HANDOFF.md" },
    { "role": "thinkersjournal-community", "order": 2,
      "cwd": "C:/Projects/ThinkersJournal-Community", "name": "thinkersjournal-community",
      "model": null, "mode": "auto", "channels": ["server:claude-peers"],
      "handoff": "C:/Projects/ThinkersJournal-Community/HANDOFF.md" },

    { "role": "humboldt", "order": 2,
      "cwd": "C:/Projects/HumboldtUnifiedKidTracker/.claude/worktrees/backend-foundation",
      "name": "humboldt", "model": "claude-opus-5-5", "mode": "auto",
      "channels": ["server:claude-peers"], "handoff": null,
      "notes": "CHECK: launch cwd is a worktree; HumboldtUnifiedKidTracker has no root HANDOFF.md" },

    { "role": "coderipper", "order": 3, "cwd": "C:/Projects/coderipper", "name": "coderipper",
      "model": "sonnet", "mode": null, "channels": ["server:claude-peers"], "handoff": null,
      "notes": "Running copy has no -n, so it cannot be recognised by name until relaunched with one. No HANDOFF.md." },
    { "role": "kiss", "order": 3, "cwd": "C:/Projects/KISS", "name": "kiss",
      "model": "sonnet", "mode": null, "channels": ["server:claude-peers"], "handoff": null,
      "notes": "Running copy has no -n. No HANDOFF.md." },
    { "role": "auth-framework", "order": 3, "cwd": "C:/Projects/auth-framework",
      "name": "auth-framework", "model": null, "mode": null,
      "channels": ["server:claude-peers"], "handoff": null,
      "notes": "Running copy has no -n and no --model. No HANDOFF.md. Shares cwd with auth-framework-deps." },
    { "role": "auth-framework-deps", "order": 3, "cwd": "C:/Projects/auth-framework",
      "name": "auth-framework-deps", "model": "sonnet", "mode": null,
      "channels": ["server:claude-peers"], "handoff": null,
      "notes": "CHECK: cwd taken from its peer (bambxhyq), not from a state file (it has none)." },

    { "role": "agentlife", "enabled": false, "order": 3, "cwd": "C:/Projects/agentlife",
      "name": "agentlife", "model": "sonnet", "mode": null,
      "channels": ["server:claude-peers"], "handoff": null,
      "notes": "The lane that is writing this. Disabled until CireSnave says whether a dev lane restores." }
  ]
}
```

### What the example shows

- **Both lanes sharing `C:/Projects/auth-framework` need distinct `name`s** (they have them); a roster
  keyed by cwd could not hold both. Today only `-n auth-framework-deps` carries a name.
- **`mode: null` for 6 lanes** is "no flag on the command line", not "default mode proven". Whether a
  lane actually ran in another mode via Shift+Tab is invisible to every source I read (DESIGN §0.5), so a
  person must say what each should be. Restoring them at default may be *narrower* than they ran.
- **`model`:** the process table shows aliases (`sonnet`) for hand-started lanes and full ids for
  tool-launched ones. The example keeps what each command line says; a person should normalise.
- **Four lanes have no HANDOFF file** at their repo root (`coderipper`, `KISS`, `auth-framework`,
  Humboldt, by `ls`); `handoff: null` makes the report say so rather than guess a path.
- **Humboldt's launch directory is a worktree.** If worktrees come and go, a roster pointing at one
  breaks silently; `restore` must check the directory exists and report, not create it.

## Validation, run on load and before every restore

1. Parses; `schema == 1`; no unknown keys.
2. `role` unique (case-insensitively); `(cwd, name)` unique; names/roles match the charset.
3. Exactly one entry has `order: 0` and its role is `pm`.
4. `mode` is `null` or a known mode; `bypassPermissions` only for `role == "pm"`.
5. `cwd` exists, is under `C:/Projects`, contains no `;`; no element of any argv contains `;`.
6. `channels` ⊆ `{"server:claude-peers"}` and `extra_args` empty (v0.1; widened later by signed entry).
7. Failure of any rule for one lane skips **that lane** and reports it; failure of 1–3 refuses the file.

## Runtime companions (not part of the roster)

`C:/Projects/.agentlife/observed/<role>.json` (last verified `pid`, process start time, peer id,
session id), `reports/restore-<UTC>.json`, `agentlife.log`, `restore.lock`, `roster.last.json` (the copy
the previous restore ran, for the change-detection in `V0.1.md`).
