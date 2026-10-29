# Task Scheduler tasks: logon and unlock

`agentlife install-task` prints, and with `--register` creates, two per-user tasks (module `src/task.rs`).
Both are "run only when the user is logged on" (`InteractiveToken`, `LeastPrivilege`, no stored password, no
elevation) because a `wt.exe` tab needs the desktop. There is no execution time limit.

| Task | Trigger | Runs |
|---|---|---|
| `agentlife-logon` | `LogonTrigger` | `agentlife restore --from-logon` |
| `agentlife-unlock` | `SessionStateChangeTrigger` = `SessionUnlock` | `agentlife pending --prompt` |

DESIGN-REVISION-2 §6.3 says "the same task" carries the unlock trigger. One task has one action list and
these are two commands (an unlock must never start a restore), so they are two tasks.

`restore --from-logon` waits up to `network_wait_secs` (default 120, polling every 5 s) for: `gh api rate_limit`
succeeding, the claude-peers broker answering, and the `lane-restart` host program being present. Being
unready is reported (naming what is missing) but does not stop the command; it then does what `restore` does,
which today is to refuse (no consent backend). `pending --prompt` exits 0 silently with nothing pending; with a
pending restore it says it cannot ask (no backend / no prompt wired) and exits 1.

`--register` replaces same-named tasks (`/F`); `--remove` deletes both. Tests never register a task.

## Measured: can the unlock trigger be created without elevation? (2026-10-08)

Yes. Non-elevated shell (`IsInRole(Administrator)` = False, `Mandatory Level` = Medium), Windows 11 Pro
10.0.26200, agentlife 0.2.12 dev build. The XML printed by `agentlife install-task` was registered with the
action replaced by `cmd.exe /c exit 0`, under throwaway names `agentlife-probe-logon` and
`agentlife-probe-unlock`:

```
schtasks /Create /TN agentlife-probe-unlock /XML <utf-16 file> /F   -> SUCCESS ... created.   exit 0
schtasks /Query  /TN agentlife-probe-unlock /XML  -> contains <SessionStateChangeTrigger> ... <StateChange>SessionUnlock</StateChange>
schtasks /Delete /TN agentlife-probe-unlock /F    -> SUCCESS ... deleted.   exit 0
schtasks /Query  /TN agentlife-probe-unlock       -> ERROR: The system cannot find the file specified.  exit 1
```

The logon probe behaved the same (`<LogonTrigger>` present after create). The final failing query is the
control that the delete took effect. **Not measured:** that the unlock trigger actually *fires* on a real
unlock, and that `wt.exe` resolves from a task-launched process. Those are M6 (`docs/ACCEPTANCE.md`).

## The `synapsed` task (`scripts/synapsed-task.ps1`)

Not an `agentlife` command: a script that starts the Synapse daemon at logon with a fixed environment
(Synapse spec `2026-10-15-daemon-contract-and-pilot.md` section 1; agentlife owns the task). Dry run by
default (prints the wrapper and the XML); `-Register` creates the task `synapsed-<user>` (one per account), `-Remove` deletes it
(no `-Exe` needed). The action is `cmd.exe /d /c <wrapper>`; the wrapper is a new timestamped file per registration, in a
log directory locked to the registering user with `icacls`; paths must be absolute ASCII and the port 1-65535.
Per-user and interactive like the two above. The task runs a wrapper `synapsed-run.cmd` that SETS
`SYNAPSE_HOME`, `SYNAPSE_ADDR` (loopback only, checked) and `SYNAPSE_LAUNCHER_ROLES` (empty until the launch
poller is live), plus `SYNAPSE_MAILBOX_STORE` only when `-MailboxStore <value>` is passed and `SYNAPSE_LAUNCH_SENDERS` only when `-LaunchSenders <value>` is passed (synapsed rc.178; each unset by default: no line; same cmd-metacharacter refusal), appends stderr to `synapsed.log`, and maps `synapsed` exit 2 (refused to start: config, lock,
second instance) to task exit 0 so Task Scheduler's restart-on-failure (3 times, 1 minute apart) never loops on
it. Any other non-zero exit is restarted.
**Known limit:** if a lane's auto-start wins the instance lock before the task, the task's `synapsed` exits 2, the task
completes, and nothing restarts the lane-started daemon if it later dies (exit 2 cannot be told from any other lock
problem). The task runs at logon, before lanes launch, so this needs a lane to start first.

Measured 2026-10-16 (no task registered): dry run prints well-formed XML (parsed, `RestartOnFailure` count 3);
the wrapper with a stand-in child returned 0->0, 1->1, 2->0, 3->3 and logged the exit-2 line; a non-loopback
`-Addr` is refused. **Not measured:** registering the task, that it fires at logon, that a real `synapsed`
starts under it, and that restart-on-failure fires on a crash. Those are the PM's T-0 run.
