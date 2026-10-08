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
