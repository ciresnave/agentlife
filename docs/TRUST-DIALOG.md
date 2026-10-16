# Answering Claude Code's folder-trust dialog

CireSnave, 2026-10-14, verbatim: *"I always trust our own projects because the only two people working on them are you and I."*
This is the mechanism that lets a relaunched lane's trust dialog be answered **for our own project folders only**. It is
a handler in the ordinary approval format (RESTART-TOOL-DESIGN.md section 12): the record lives in CireSnave's approvals
repo (`ciresnave/ciresnave`), where **his merge is the approval**; this repo ships none (`approval-example/` is inert).

## What the dialog looks like

OverMind's captures, claude 2.1.301 (pyte, 2026-10-16; the same text as 2026-09-09), verbatim except the border:

```
────────────────────────────────  (U+2500 x the screen width)
 Accessing workspace:

 <path>

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source
 project, or work from your team). If not, take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
```

- **Every text line has one leading space.** `match.lines` trims surrounding whitespace, so the pin is the bare text.
- **The default selection is "No, exit".** Only `<Down>` then `<Enter>` answers Yes (`x`, `y` do nothing; `2<Enter>` exits).
  The action is `<Down><Enter>` (`handlers::action_bytes`: `ESC [ B` then `\r`).
- **The path is hard-wrapped at the screen edge, mid-word**, when it does not fit: at 80 columns a 116-character path
  became `" C:\...\a-fairly-long-folder-name-"` then `" to-test-wrapping"` (the continuation also has the one leading
  space). The quick-safety paragraph is word-wrapped and re-wraps with the width, so it is never pinned.

## The pins, and why each

```json
"match": {
  "text_anchors": ["Quick safety check"],
  "lines": ["Accessing workspace:", "Yes, I trust this folder"],
  "isolated_lines": ["C:\\Projects\\<folder>"]
}
```

- `lines` pins the two short lines that do not depend on the width.
- `isolated_lines` pins the folder: the row must equal the path **exactly** and the **next row must be blank**. A plain
  anchor is a substring (`C:\Projects\agentlife` is inside `C:\Projects\agentlife-evil`); a plain `lines` pin is exact but
  is fooled by one case: a trusted path exactly (width - 1) characters long followed by more text, where the wrap puts the
  trusted path alone on the first row. The continuation row is not blank, so `isolated_lines` is not fooled. Tested.
- **One record per lane folder, no prefix, no wildcard.** A folder that is not listed is never trusted; so is a worktree
  or a subfolder of a listed one.

## Screen width: the host's PTY size (read from `host.rs`)

`lane-restart host` opens its ConPTY at **the size of the console window it is started in**
(`current_console_size()`: the window's `srWindow` width and height), **falling back to 80 x 25** when that cannot be
read, and **resizes the PTY and its screen parser whenever the outer terminal is resized**. So the width is the real
window width, not a constant. A Windows Terminal tab is commonly 120 columns wide (not measured here).

- Path rows include a leading space, so a path of `n` characters needs a screen at least `n + 2` columns wide to avoid
  wrapping. Our longest lane folder path is `C:\Projects\ThinkersJournal-Community` and `C:\Projects\HumboldtUnifiedKidTracker`
  (37 characters): far under 80.
- **A path that wraps does not match: nothing is answered and the dialog waits for a person.** The pin fails closed.
  At 80 columns the shipped folders still fit; a path longer than 78 characters would not.
- The screen text is the parser's visible rows joined by newlines (`host::screen_text`), so the "next row is blank" rule
  reads the same rows a person sees.

## Which records

One per lane folder under `C:\Projects` (regenerate from the roster when it changes): `agentlife`, `auth-framework`,
`baracuda`, `coderipper`, `fuel`, `HumboldtUnifiedKidTracker`, `KISS`, `lightbulb`, `OverMind`, `PersistAnt`,
`SafeImage`, `synapse`, `ThinkersFoundation`, `ThinkersJournal-Community`, `Unpopped`, `unisql`. The record `id` is
`trust-folder-<folder lower-cased>`. `approval-example/trust-folder-agentlife.json` is the template and a test fixture:
the generated records are that file with the id and the path changed, and each was checked with the same
`parse_and_validate` and against the captured dialog (match at 120 columns; `-evil` suffix refused).

The secret-word veto still applies to the **screen**: a folder name containing a whole word such as `secret`, `token` or
`pin` makes the screen un-answerable, which fails closed.

## Not covered

- The dialog is answered only by the host on a session `lane-restart` relaunched, as for every handler; a person's own
  session is never touched.
- A new Claude Code build can change the text. The pins are exact, so a change means no answer, not a wrong answer.
- Nothing here writes Claude's own trust state (`~/.claude.json`); it only answers the dialog.
