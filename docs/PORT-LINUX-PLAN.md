# Linux port plan: lane-restart, the lane host, and the consent/secret broker

Status: **in progress** (updated 2026-10-14 at `origin/main` = `d9605ba`, 0.14.28). Built: **P0, P1, P2**
(0.14.27) and **P4** (0.14.28). Rulings since the first version: Q1, Q2, Q3 (section 7). Two slices were re-read
against the code and changed: **P3** and **P11** (section 6, "Revisions"). Original status: proposal, no code,
written 2026-10-14 at `origin/main` = `a84165d` (0.14.26). Standing direction
(PM message 2026-10-14 17:12Z, relaying CireSnave): *AgentLife (lane-restart, lane host) and the secret broker must
not be Windows-only; the goal is Windows AND Linux.* The PM asked for: an inventory of every Windows-only seam, a
trait plus per-OS backend proposal, and a sized slice list, as a docs PR; **no code before this plan merges**.

## 1. Where we start from (read, not assumed)

* **CI already builds, lints and tests on Linux.** `.github/workflows/ci.yml` runs `fmt`, `clippy -D warnings`,
  `build` and `test` on `ubuntu-latest` and `windows-latest` (`fail-fast: false`). So the port is about making the
  *behaviour* real on Linux, not about compiling.
* **The terminal layer is already portable in the library**: `lane-restart host` uses `portable-pty` 0.9.0
  (ConPTY on Windows, `openpty` on unix) plus `vt100`. Only the host's raw console mode and size query are Windows.
* **OS code is concentrated behind traits**: `SystemFacts` (lane-state), `ProcessTable` (identity), `Spawner`
  (launch), `Terminator` (down), `HookEnv` (hook), `Prompt`/`Consent` (consent). The seams exist; most need a second
  implementation, not a redesign.
* **The broker is a separate crate**: `user-request` 0.11.1 (OverMind, crates.io) owns DPAPI and Windows Hello. It is
  not in this repository. See section 5 for who must change what.
* No `cfg(target_os)` anywhere; the platform split is `cfg(windows)` / `cfg(not(windows))` / `cfg(unix)`.

How this was measured: an exhaustive read of `origin/main` by a subagent (Sonnet), then **10 of its claims re-read
by me against the tree** (paths and quotes below marked ✔). Everything marked INFERRED was not run on Linux. This
plan has not been tried on a Linux machine; section 8 lists what must be measured there first.

## 2. Inventory of Windows-only seams

`✔` = re-read by me at `a84165d`. File:line are at that commit.

### 2.1 Terminal and PTY hosting

| Seam | Where | On Linux today | Fix |
|---|---|---|---|
| Raw console mode (VT input, no line buffering) so the terminal's cursor-position reply reaches the child | `crates/lane-restart/src/host.rs:54-123`, used at `:613` (`cfg(windows)`) | **absent**: stdin stays cooked, the CPR reply never arrives (the exact Windows bug documented at `:43-53`) | unix guard: `termios` raw on stdin, restore in `Drop` |
| Console size | `host.rs:770` windows / `:787` ✔ `cfg(not(windows))` returns `None` | PTY is always 80x25, resize forwarding never fires | `ioctl(TIOCGWINSZ)`; SIGWINCH instead of the 500 ms poll |
| Open a terminal tab for a launch | `src/launch.rs:263-264` ✔ (`wt.exe`, `conhost.exe`), `wt_args` `:203-212`; `crates/lane-restart/src/relaunch.rs:491,497` ✔ | `NotFound` twice, no session can start | `Spawner`/launch backend per OS (section 4) |
| `SpawnVia` is persisted in reports | `src/launch.rs:215-223` | new variants needed (`windows_terminal`/`conhost` are stored values) | additive enum variants |
| ConPTY teardown workaround | `host.rs:402-433,511-546` | harmless extra wait possible (INFERRED) | measure, then trim |
| `;` rejected in arguments (wt's separator) | `src/launch.rs:136-142` | over-restrictive but safe | gate by terminal kind |
| `windows-sys` is an unconditional dependency | `crates/lane-restart/Cargo.toml:18` ✔ | compiles to nothing; noise | move under `[target.'cfg(windows)'.dependencies]` |

### 2.2 Process table, identity, liveness

| Seam | Where | On Linux today | Fix |
|---|---|---|---|
| Start-time identity is whole seconds from `sysinfo` (boot time + start tick) | `src/identity.rs:9-14,60-65`; issue #14 | can differ by 1 s between reads, so a live process reads as `RecycledPid`/`NotRunning` (documented, **unproven**: `ROADMAP.md` item 6) | read `/proc/<pid>/stat` field 22 (ticks) with `btime`, compare ticks; no tolerance |
| Same value as a file key | `src/procindex.rs:30-32` (`{pid}-{start_secs}.id`) | SessionEnd lookup can miss under drift | key on the tick value |
| Claude is recognised by image name `claude` / `claude.exe` | `crates/lane-state/src/facts.rs:180` ✔, `claude_proc.rs:220`, `duplicate.rs:85`, `hook.rs:275`, `caller.rs:36` | **if Claude Code runs as `node` or another name on Linux, every liveness check, parent walk and the double-launch guard fail** (INFERRED; section 8 item 1) | one `ClaudeImage` predicate fed by exe path or `cmd[0]`, configurable |
| Shell names | `claude_proc.rs:198` ✔ (`bash,sh,cmd,powershell,pwsh`), `hook.rs:41` | `zsh`/`fish`/`dash` make `claude_parent_pid` return `UnexpectedAncestor` and the hook refuses | one shared list incl. zsh, fish, dash, ksh |
| Terminal-host names | `hook.rs:42` ✔ (`windowsterminal,openconsole,conhost,wt`), `tab_close.rs:26,28` | a hand launch is `Origin::Other`, never `Hand`; leftover-tab close never fires | add emulators/tmux/sshd, or skip the feature (section 4) |
| Kill | `facts.rs:263-298`, `down.rs:111-146` use `Process::kill()` | SIGKILL, no graceful TERM, children survive (no process group) | TERM, grace, KILL; signal the process group/session |
| Path normalisation | `src/identity.rs:109-123`, `paths.rs:22-50`, `disk.rs:98-102` | `\` treated as a separator, `disk.rs` always lowercases (`/Data` == `/data`) | `cfg`-split the Windows-only rules |
| Disk floor | `crates/lane-restart/src/disk.rs:119-143` | `C:/Projects/.lane-state` is under no mount, so the floor is `Unknown` and **silently unchecked** | resolve the real state dir; keep `Unknown` loud |
| Reboot-vs-crash marker | `src/intent.rs:266-289` (`wevtutil` events 1074/6006), `cfg(not(windows))` returns `None` | every ambiguous case becomes a candidate | `journalctl`/`last -x` marker |

### 2.3 Secrets, consent, user presence (all through `user-request` 0.11.1)

| Seam | Where | On Linux today |
|---|---|---|
| Key at rest: DPAPI `CryptProtectData` with per-tool entropy | `user-request/src/dpapi.rs:16-75`; non-Windows `impl Protector` returns `Err("DPAPI is Windows-only")` | **every consent call fails closed** (documented: `docs/CONSENT.md:35`) |
| Approval: Windows Hello `UserConsentVerifier` | `user-request/src/hello.rs:23-81`; non-Windows returns `Unavailable` | nothing can ever be approved |
| Store directory `%LOCALAPPDATA%\OverMind\user-request` | `user-request/src/locate.rs` | `Err("LOCALAPPDATA is not set")` |
| Audit-head copy `C:/Projects/.lane-state/user-request-audit.head` hard-coded | `user-request/src/locate.rs` | relative path under the cwd |
| Our wiring | `src/consent/real.rs:130-137` ✔ (`locate::dir()`, `head_copy()`, `PROTECTOR`), `:342-397` (`HelloPrompt`), `src/main.rs:281,305` | the only place Hello is put in front of a person |

### 2.4 Scheduling and autostart

`src/task.rs:75-137` (Task Scheduler XML, UTF-16LE, `<LogonTrigger>`, `SessionUnlock`), `src/main.rs:716-720` ✔
(`--register/--remove` refused off Windows), `:731-738` (`USERDOMAIN`/`USERNAME` required even for print mode),
`:781-801` (`schtasks`). The design also assumes a desktop session ("a `wt.exe` tab needs the desktop").

### 2.5 Paths and filesystem

| Constant | Where | On Linux |
|---|---|---|
| lane-state dir | `crates/lane-restart/src/lib.rs:46` ✔, **plus two more copies** at `host.rs:556` ✔ and `:700` ✔ | relative directory `C:/Projects/.lane-state` created under the cwd, silently |
| AgentLife home | `src/home.rs:14` ✔ `C:/Projects/.agentlife` (`AGENTLIFE_HOME` override exists) | same |
| portfolio root, hooks dir, hook exe | `src/config.rs:191,193`, `src/main.rs:634`, `src/doctor.rs:19`, `install.rs:26` | same |
| exe name | `crates/lane-restart/src/install.rs:19` ✔ `lane-restart.exe` (and the `.bak`, sha line) | installs a file literally named `.exe` |
| role from cwd | `lane_state_writer.rs:28-38` | `Path::file_name` does not split a Windows path; Linux-native cwds are fine |
| `restore::resolve_program` | `src/restore.rs:344-365` | no executable-bit check (INFERRED minor) |

Already fine: atomic write by rename, `create_new` locks, `rename` over a running exe (still the right approach on
Linux), `HOME` fallback in `home_dir`, `.claude` dir resolution.

### 2.6 Tests and examples that are Windows-only

* The **whole PTY host is untested on Linux CI**: 11 real-ConPTY tests are `#[cfg(windows)]`
  (`host.rs:1008-1642`); on Linux only the pure helpers run. (Whether their child commands are Windows-only or they
  merely never ran on Linux is unread; section 8 item 3.)
* `tests/launch_e2e.rs:740,769` (real conhost / real Windows Terminal, opt-in env), `src/install.rs:157`
  (running exe cannot be overwritten, uses `PING.EXE`), `src/intent.rs:716` (event log).
* Consent tests assert only the fail-closed message off Windows (`src/consent/real/tests.rs:579`,
  `tests/hook_e2e.rs:1592`).
* Most unit tests carry `C:/Projects/...`, `claude.exe`, `WindowsTerminal.exe` as data and so pass on Linux
  without exercising POSIX paths or `/proc` names.

## 3. Design: one platform seam, two backends

Principle: **Windows behaviour stays byte-for-byte what it is**; the first slices only move constants and lists
behind a function with the current Windows value as its Windows result. A Linux backend is then additive.

Where it lives: **a `platform` module in `lane-state`** (the crate `lane-restart`, `agentlife` and `with-secret`
already share; the workspace has one version number, so a fourth crate buys nothing). Alternative considered:
a new `lane-platform` crate. Rejected for now; revisit if `lane-state` starts pulling in `libc`/`nix` for
consumers that do not want them (then gate by feature).

```text
platform::dirs        state_dir(), agentlife_home(), hooks_dir(), portfolio_root()   // env > config > OS default
platform::names       claude_image(&ProcEntry)->bool, shell_names(), terminal_host_names(), exe_suffix()
platform::identity    start_token(pid)->Token   // Windows: whole seconds (today); Linux: /proc/<pid>/stat ticks
platform::signal      terminate(pid_identity, grace) // Windows: TerminateProcess (today); unix: TERM, grace, KILL, pgrp
platform::tty         RawGuard::enable(), console_size()  // Windows: console modes; unix: termios + TIOCGWINSZ
platform::launcher    trait TabLauncher { fn open(&self, spec) -> Result<Opened> }  // backends below
platform::scheduler   trait Autostart  { fn describe/ register/ remove }              // schtasks | systemd --user
platform::shutdown    fn last_shutdown_marker(boot) -> Option<..>                    // wevtutil | journalctl
```

Defaults on Linux (all overridable by the env vars/config that already exist, never by a CLI flag for the state
dir, keeping the existing "one fixed place" rule):

* state dir: `$XDG_STATE_HOME/agentlife/lane-state` (fallback `~/.local/state/...`), single resolver used by every
  caller, replacing the **three** copies;
* AgentLife home: `$XDG_STATE_HOME/agentlife`; hooks dir: `~/.local/lib/agentlife/hooks`.

### Launch backends (ruled, section 7 Q1: direct first, tmux later, no emulator)

| Backend | How a lane gets its session | Pros | Cons |
|---|---|---|---|
| `direct` | spawn `lane-restart host ... -- claude ...` detached (own session, `setsid`) with the host's PTY the only terminal | works headless and over SSH; nothing to find or close | no visible tab; a human attaches via a socket/`lane-restart attach` (new) |
| `tmux` | `tmux new-window -d -n <role> ...` in a named session | headless, attachable, close = `kill-window`; replaces the whole tab-close feature | needs tmux; a second terminal layer under the host's PTY |
| `emulator` | `x-terminal-emulator` / `gnome-terminal` / `kitty` / `konsole` with a per-terminal arg builder | closest to today's visible tab | needs a display session; each emulator differs (`--title`, `--working-directory`, `--`); window-per-agent only |

**Ruled (CireSnave, 2026-10-14, board 205 Q1, verbatim):** *"Direct process first with tmux support on our
roadmap to build later."* So: `direct` is the Linux launch; `tmux` is a later viewer (a ROADMAP row, not
started); **`emulator` is dropped** (no terminal-emulator support is planned). Windows keeps `wt.exe` ->
`conhost.exe`. The host's tty handling (P4) depends on neither tmux nor an emulator.

## 4. Process model on Linux

* **Identity**: tick-based start token (`/proc/<pid>/stat` field 22 + `btime`), compared exactly. This is the fix for
  issue #14 if (and only if) drift is what a failing log shows; it is also simply the more accurate value, so it is
  worth doing before the flake is proven. Keep "no tolerance" (the PM's ruling).
* **Claude image**: do not hard-code `claude`. Match on the resolved exe path or `cmd[0]` basename, with a
  configurable list, and **record what the Linux install actually runs as** (section 8 item 1).
* **Stop**: `SIGTERM`, bounded wait, then `SIGKILL`, to the process group of the host so the `claude` child and its
  shells go with it. This is stricter than `sysinfo::Process::kill()`.
* **Sessions**: the host calls `setsid` on `direct` launches so a closing terminal cannot take lanes with it.

## 5. The broker (`user-request`): who changes what

`user-request` and `with-secret` belong to OverMind; AgentLife only consumes `user-request` 0.11.1 from crates.io
(`src/consent/real.rs`). **This repository cannot edit them.** What we *can* do here is make our side ready:

1. our wiring already goes through two traits (`Prompt`, `Consent`) and `UserRequestBackend::production`; a
   `with_parts(...)` constructor already exists in the backend (`src/consent/real.rs:141`; the arguments were not
   re-read in full, confirm in P11), so a Linux
   `Protector` can be supplied **without** an upstream change once one exists;
2. the missing pieces are all upstream: Linux `Protector`, a Linux approval channel, XDG store dir, and the
   audit-head path. These become **upstream requests** with the acceptance below; the PM routes them.

### Key at rest (`Protector`)

DPAPI user scope protects against disk copies and backups, **not** against any process running as the user
(`dpapi.rs` header says so). The Linux options below differ in the first half of that sentence: the key
file does **not** survive a copy of the disk (see its row and the correction after the table).

| Option | Survives reboot | Needs | Notes |
|---|---|---|---|
| Secret Service via D-Bus (`keyring` crate: gnome-keyring, KWallet) | yes | a running, unlocked session keyring | fails on headless/SSH-only; unlock prompt is a second consent surface |
| Kernel keyring (`keyctl`, user keyring) | **no** | nothing | good as a cache, not as storage |
| TPM2-sealed key (`tpm2-tss`, `/dev/tpmrm0`) | yes | TPM, user in `tss` group | strongest at rest; hardware-bound; absent on many VMs |
| `0600` key file under the state dir | yes | nothing | **weaker than DPAPI user scope** (corrected 2026-10-14, see below): the KEK sits beside the store and is copied with it |

Recommendation: a **tiered `Protector`**: TPM2 if present, else Secret Service if reachable, else `0600` file,
and the chosen tier is written into the store header and printed by `user-request list`, so no tier is silent.
Do not pick the file tier without saying so.

**Correction (2026-10-14, PM task, from OverMind's own audit in `consent` `docs/LINUX-PORT-DESIGN.md` sections 2
and 6):** an earlier version of this plan called the file tier "parity with DPAPI user scope". It is not. DPAPI
user scope keeps the key out of the copied files, so a copy of the disk or a backup does not carry the key; a key
file in the same state tree is carried by that same copy, and any same-user process can read it. It separates
users by file permission and nothing more. It is the tier of last resort, shown on every `list`, never hidden.
Secret Service and TPM2 keep the key out of the copied tree (a same-user process can still ask them to unseal it).

### Approval channel (the Hello equivalent)

The property to preserve is stated in `hello.rs`: *"the ONE approval channel no agent can answer."* On Linux the
agent runs as the same user, so **anything a same-user process can type, click or read is not such a channel**:
a TTY `y/n`, a `pinentry-curses` on a pty the agent can open, X11 (XTEST can synthesise input) all fail that bar.
Candidates that can pass it:

| Channel | Answerable by an agent? | Needs |
|---|---|---|
| FIDO2/U2F key, user-presence touch (`libfido2`) | no (physical touch). **But a touch proves a person touched the key, not that they read the text**: a key has no screen, so the prompt is shown on a surface a same-user agent may influence (the signature binds request, text hash and a fresh nonce, which defeats replay, not misdirection). Weaker than Hello; OverMind's design asks CireSnave to accept that explicitly | a security key; works headless |
| `fprintd` fingerprint verify (D-Bus `net.reactivated.Fprint`) | no (biometric) | reader + enrolled finger |
| polkit authentication agent (password/biometric dialog by a separate agent process) | **uncertain** on X11 (synthetic input), harder on Wayland | desktop session |
| TPM2 `authValue` PIN on an unseal | no, if the PIN is typed by a person on a path the agent cannot read | TPM + a PIN entry surface (again a UI to secure) |

**Ruled (CireSnave, 2026-10-14, board 205 Q2, relayed by the PM; not a verbatim quote): FIDO2 touch, tiered key,
no terminal prompt.** Recommendation as written: **FIDO2 touch as the required channel**, with fprintd as an alternative where a reader exists;
**no terminal-prompt fallback**, ever (it would silently turn "a person approved" into "something on this machine
answered"). If none is available the result stays `Unavailable` and consent fails closed, as it does today.
This is a security-model choice, so it needs CireSnave and the OverMind owner, not this repo alone (Q2).

### Exact asks for the upstream (`user-request`, OverMind) - for the PM to file as P12 and P13

Each ask names the symbol read in 0.11.1 and the acceptance a Linux CI leg can check. Nothing here changes the
consent model (who / what / duration, one-shot grants, hash-chained audit).

| # | Ask | Today (0.11.1) | Acceptance |
|---|---|---|---|
| U1 | `locate::dir()` returns an XDG path off Windows: `$XDG_STATE_HOME/OverMind/user-request`, else `~/.local/state/OverMind/user-request`; `USER_REQUEST_DIR` stays debug-only | `%LOCALAPPDATA%` only; `Err("LOCALAPPDATA is not set")` | `dir()` is `Ok` on Linux with and without `XDG_STATE_HOME`; the Windows value is unchanged |
| U2 | `locate::head_copy()` stops hard-coding `C:/Projects/.lane-state/...`: use the same state-dir resolver as `lane-state` (single definition), `USER_REQUEST_HEAD` stays debug-only | literal `C:/Projects/.lane-state/user-request-audit.head` (relative path under the cwd on Linux) | on Linux the head copy is outside the store dir, absolute, and verified-to-exist on first write |
| U3 | A Linux `Protector` behind `locate::PROTECTOR`, **tiered** TPM2 -> Secret Service -> `0600` key file, the chosen tier recorded in the store header and shown by `user-request list` | `Dpapi` non-Windows impl returns `Err("DPAPI is Windows-only")` for both `protect` and `unprotect` | round trip on Linux CI at the file tier; a blob written by one tier is refused (not silently re-wrapped) by another; the tier is never silent |
| U4 | `cfg` hygiene: `windows`/`windows-future` stay `[target.'cfg(windows)']`; new Linux deps (`keyring`, `tpm2`, `libfido2` bindings) behind features so a headless build needs none | n/a | `cargo tree` on Linux without features pulls no D-Bus or TPM crate |
| U5 | A Linux `Consent` (`hello.rs` counterpart) whose success requires **physical presence**: FIDO2 user-presence touch first, `fprintd` verify second. Returns `Unavailable` (never `Approved`) when neither exists | `HelloConsent::ask` returns `Unavailable("Windows Hello exists only on Windows")` | a fake authenticator test proves `Approved` only on a presence assertion, `Denied` on cancel, `TimedOut` on the deadline; a **live** test (ignored by default, like `live_hello_cancel_is_denied`) |
| U6 | **No terminal-prompt channel** in the crate, behind no flag and no env var | n/a | a grep-level test that no `Consent` impl reads stdin; documented in the crate README as a deliberate omission |
| U7 | `Owner`/window parameters of `HelloConsent` do not leak into the portable API: `Channel` construction takes only what every OS needs | `Owner::{Foreground,Console}` are Windows window handles | the same `HelloChannel::new` call site compiles on both OSes |
| U8 | `with-secret` consumes U1-U3 unchanged (it shares the store) and states its Linux support level in its own README | shares `locate` | `revoke --all` on Linux covers every kind |

AgentLife's side (P11) is only the injection seam and tests with a fake protector and prompt; it needs none of
U1-U8 to land first, and it must not guess their names before they are published.

## 6. Slice list

Sizes: **S** small PR (under about 300 changed lines, no new dependency), **M** medium, **L** large or needs a
decision first. "Owner": this repo unless marked. Every slice keeps Windows behaviour identical and carries a
Windows leg and a Linux leg in CI.

| # | Slice | Size | Depends | Proof it works |
|---|---|---|---|---|
| P0 | **Constants resolve per OS**: one `platform::dirs` replacing the three `C:/Projects/.lane-state` copies, `DEFAULT_HOME`, hooks dir, `HOOK_EXE`, `portfolio_root`, `worktrees --stale` root; Windows value unchanged | S | none | test per resolver; a Windows run diff-clean; Linux no longer creates `C:/...` under the cwd |
| P1 | **Manifest and naming hygiene**: `windows-sys` under `[target.'cfg(windows)']`, `EXE_SUFFIX` in install names and sha line, reword `wt.exe` text | S | none | `cargo tree --target x86_64-unknown-linux-gnu` has no `windows-sys` for lane-restart |
| P2 | **Name sets**: one `platform::names` (claude image predicate, shells incl. zsh/fish/dash, terminal hosts); replace the five copies | S | P0 | table-driven tests; a real `zsh` ancestor accepted |
| P3 | ~~Start-time token on Linux (`/proc` ticks)~~ **replaced by P3'**, see "Revisions" below | n/a | n/a | n/a |
| P3' | **Identity-mismatch diagnostic (Linux)**: on a recycled-pid / identity-changed / not-live verdict, record sysinfo `start_time`, `/proc/<pid>/stat` ticks and `btime` on the ledger line | S | none | a test with a stand-in table; the failing log issue #14 is waiting for |
| P4 | **Host tty on unix**: raw guard, `TIOCGWINSZ`, SIGWINCH; un-gate the 11 ConPTY host tests with Linux children | M | none | real host test: CPR round trip and resize on Linux CI |
| P5 | **Signals**: TERM, grace, KILL to the process group; `down` and `lane_stop` | S | P3 | tests with real `sleep` trees: children gone, identity-mismatch refused |
| P6 | **`TabLauncher` trait + `direct` backend** (`setsid`); `SpawnVia::Direct` gets real use; report fields additive. **This is the Linux lane host (Q1)**; a way to look at a running lane (`lane-restart attach`) belongs here, over the host's own pty, with no tmux | M | P0, P4 | `launch_e2e` Linux leg opens N agents and the registry shows them |
| P7 | ~~`tmux` backend~~ **tmux viewer, LATER (ROADMAP row, not started)**: a viewer over the direct host, never a launcher the host depends on | M | P6 | real tmux test (skipped, loudly, when tmux is absent) |
| P8 | **Autostart**: `Autostart` trait, `systemd --user` units for logon, an unlock path (`logind` `Unlock` signal) or a documented "logon only" limit | M | P6 | `install-task` prints units; a real `systemctl --user` test on CI where available |
| P9 | **Shutdown marker** via `journalctl`/`last -x` | S | none | classifier test with captured sample output |
| P10 | **Hook origin / tab-close lists** use the P2 sets; `Origin::Hand` reachable on Linux | S | P2 | hand-launch test with a Linux terminal ancestor chain |
| P11 | ~~Consent readiness on our side~~ **already built**, see "Revisions" below | n/a | n/a | n/a |
| P12 | **Upstream (OverMind): Linux `Protector` (tiered) + XDG store dir + head path** | L | decision Q2 | `user-request` round trip on Linux CI, tier shown in `list` |
| P13 | **Upstream (OverMind): approval channel** (FIDO2 first) | L | Q2, P12 | a person-present live test (like `live_hello_cancel_is_denied`), never in CI |
| P14 | **CI**: a Linux job running the real-behaviour tests (P4, P6, P7) with `tmux`/`setsid`; keep the matrix | S | P4, P6 | the job fails if a gated test silently skips |
| P15 | Docs: README/DESIGN/CONSENT/HOOK-INSTALL gain Linux sections; remove "Windows" from crate keywords | S | all | review |

### Revisions (2026-10-14, after reading the code and measuring)

* **P3 was wrong as drafted.** `*_start_secs` is used as *epoch* seconds, not only as an equality key:
  `lane_stop.rs:168` compares a session's start with a transcript mtime, `intent.rs:336` stores
  `started.timestamp()`, `facts.rs:348` filters `find_claude_process_in` by a wall-clock kill moment (with a 5 s
  margin), and `tab_close`/`hook` order parent against child by it. A boot-relative `/proc` token would silently
  break those, and the value is persisted in state files and registry records.
  **Measured** on WSL2 (Linux 6.18, `CLK_TCK` 100): ten reads of `/proc/stat` `btime` over ten seconds were
  identical. The kernel's boot time is the realtime clock minus a fixed offset, so it moves only when the clock is
  *stepped* (VM resume, an NTP step: plausible on a hosted runner at start). Issue #14 stays unproven and I have no
  failing log. So **P3' only adds the diagnostic** that would produce one; a token redesign waits for that log.
* **P11 is already built.** `UserRequestBackend::with_parts(dir, head, Box<dyn Protector>, requester)` is at
  `src/consent/real.rs:141`, its tests run a test protector on both CI legs, and `Prompt`/`Consent` are traits. The
  one hard-wired call is `production()` -> `ur::locate::*`, which cannot change until U1-U3 are published.
* **P5** no longer depends on P3 (it depended on a token it does not need).

Suggested order: **P0, P1, P2** (no behaviour change, unblocks everything), then **P3, P4** in parallel, then
**P5, P6**, then **P7, P8, P9, P10, P11**; **P12/P13** run in OverMind in parallel from the moment Q2 is answered,
because the secret broker is on the critical path for `restore` on Linux. P14 grows as each backend lands.

## 7. Decisions needed (not guessed here)

* **Q1 (CireSnave): where do lanes run on Linux?** **Ruled 2026-10-14 (verbatim):** *"Direct process first with
  tmux support on our roadmap to build later."* No terminal-emulator support planned.
* **Q2 (CireSnave + OverMind owner): the approval channel and the key tier.** **Ruled 2026-10-14 (relayed by the
  PM):** FIDO2 touch, tiered key, no terminal prompt.
* **Q3 (PM): who files and owns the `user-request` upstream work** (P12, P13). **Ruled:** the PM files U1-U8 and
  routes them to the owners; this repo cannot edit the crate.
* **Q4 (PM): macOS** is out of scope here (no `launchd`, Keychain, Touch ID backend). **Accepted.**

## 8. What is not known yet (measure on Linux before P2/P3 are final)

1. **What Claude Code's process is called on Linux** (`claude`, `node`, a wrapper). Every liveness check depends on
   it. First task on a Linux box: `ps -eo pid,comm,args | grep -i claude`.
2. Whether `persistant` 0.4.4's filesystem backend (`src/atomic.rs`) behaves on Linux (tests run on CI; behaviour
   unread).
3. Whether the 11 gated host tests spawn Windows-only children or only never ran on Linux (bodies unread).
4. `portable-pty` 0.9.0's unix `openpty`/controlling-terminal behaviour under our teardown logic.
5. Whether `sysinfo` 0.39.6 returns `cmd()`/`cwd()`/`exe()` for another user's processes (expected: same user
   only).
6. Whether Linux Claude Code derives `~/.claude/projects/<dir>` with the same rule `paths.rs:56-60` assumes.
7. `start.rs`, `stop.rs`, `lane_stop.rs`, `authorize.rs`, `handlers.rs`, `ledger.rs`, `log.rs`, `spec_import.rs`
   were scanned by pattern, not read in full.
8. The `user-request` source read was **0.11.1 only**; whether the upstream has a Linux plan is not known.

## 9. Non-goals

No behaviour change on Windows; no change to the consent *model* (who/what/duration, one-shot, audit chain); no
new approval path an agent could answer; no code in this PR.
