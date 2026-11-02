<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
<!--
Provenance (agentlife issue #175).
Origin: OverMind (github.com/ciresnave/OverMind), file USER-REPO-APP-FOLDERS-SPEC.md, as of OverMind
origin/main 2920f08 (last touched by 4746945, 2026-09-27, "lane-restart: approvals move out of OverMind;
host asks the PM on unmatched dialogs (#94)"). Author: Eric Evans (CireSnave); the idea is his, quoted
below in his words. Our copy is that file verbatim except four edits, all in this agentlife copy: the
sentence in the status line saying where the file lives, and the "Licence of this text", "Name and home"
and "lane-restart does not follow links.json" bullets in section 7 (licence now MIT OR Apache-2.0 by the
PM's 2026-10-25 ruling, home now agentlife, and the links.json check). Verified against the origin:
every other line is byte-identical.
-->

# App folders in a user's `<username>/<username>` repo — draft standard

**Status: DRAFT 0.1, 2026-09-27.** It started in OverMind's docs and now lives here, in agentlife, and
`lane-restart` (now agentlife's) is its first user (RESTART-TOOL-DESIGN.md §12). The idea is CireSnave's, in his words
(`CIRESNAVE-EXPECTATIONS.md` §5.1c):

> *"since every GitHub user has a personal repo like that which is named the same as their account, we
> should probably see if anyone has created a standard for how to organize that to store data for
> individual apps that need a communal storage space, and if no one has created that standard, we
> should create one. I'm thinking that it would make sense to have subdirectories for each app
> (possibly .OverMind/ for OverMind's settings) and, within that, have links of some form describing
> private repositories where potentially sensitive user information is stored so it is kept out of the
> public repo named after their account but can be found from that public repo."*

The key words **MUST**, **MUST NOT**, **SHOULD** and **MAY** mean what RFC 2119 says they mean.

## 1. Prior art — searched 2026-09-27; none covers this

The search found **no existing standard, proposal or tool** that uses the `<username>/<username>` repo
as a per-app settings store. It ran web searches plus `gh search repos` and `gh search code`. The
queries and hit counts are recorded in this PR's description, and every URL below was fetched
(HTTP 200) on 2026-09-27. The nearest analogues:

| Prior art | What it is | How it differs |
|---|---|---|
| [GitHub profile README](https://docs.github.com/en/account-and-profile/how-tos/profile-customization/managing-your-profile-readme) | The official definition of the `<username>/<username>` repo. GitHub shows its root `README.md` on the profile when the repo is public. | The docs cover self-presentation only. They say nothing about app data, folders or merge rules. |
| [`.github/` folder and default community health files](https://docs.github.com/en/communities/setting-up-your-project-for-healthy-contributions/creating-a-default-community-health-file) | A reserved dot-folder per repo, plus a special `.github` repo that supplies defaults for an account or org. | This is the closest structural analogue, and the reason this spec uses a dot-folder. It covers community files, not per-app user data. |
| [`repository-settings/app`](https://github.com/repository-settings/app) (was probot/settings), [`github/safe-settings`](https://github.com/github/safe-settings) | Repo and org settings as code, where changing them takes a PR. | These are an admin repo's settings for repos and orgs, not a user's per-app data. They set the precedent for "the merge is the change." |
| [Codespaces dotfiles setting](https://docs.github.com/en/codespaces/setting-your-user-preferences/personalizing-github-codespaces-for-your-account) | The user names a personal repo, and a GitHub feature reads it. | This is the nearest official case of "an app reads per-user config from a repo the user names." It is an arbitrary repo of install scripts, not app folders. |
| [dotfiles repos](https://dotfiles.github.io/), [chezmoi](https://github.com/twpayne/chezmoi), [yadm](https://github.com/yadm-dev/yadm) | One personal repo of config, usually with a subfolder per app. | These establish the per-app-subfolder habit. The repo is arbitrarily named, a local tool pulls it, and no rule covers secrets or merges. |
| [XDG Base Directory spec](https://specifications.freedesktop.org/basedir/latest/) | One directory per app under a shared config root. | This is the filesystem precedent for §2's layout. It is not about GitHub. |
| [Renovate shared presets](https://docs.renovatebot.com/config-presets/) (`github>owner/repo`) | Config that points into another repo, private ones included. | This is precedent for §4's pointer shape. It points at a dedicated config repo. |
| [`.gitmodules`](https://git-scm.com/docs/gitmodules) | A file saying "this path lives in another repo" (URL plus ref). | This is a generic pointer format with no public/private or ownership semantics. |
| [RFC 8615 `.well-known/`](https://www.rfc-editor.org/rfc/rfc8615) | A reserved, namespaced path prefix per site. | This is a naming precedent only. |
| [CODEOWNERS](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners) | Requires named owners to approve changes to given paths. | This is a mechanism §5 can use to enforce "only the user merges." |

No analogue states a "no secrets" rule for a public repo, so §6 fills a gap rather than following a
precedent. **Not verified:** very recent gists or GitHub Discussions, which web search covers thinly.

## 2. Layout

```
<username>/<username>/
  README.md                 # the profile README - apps MUST NOT modify it
  .<app>/                   # one folder per app
    README.md               # SHOULD: what this folder is, which app reads it, where to learn more
    links.json              # MAY: pointers to private repos (§4)
    <app-defined files>     # the app's own data, in the app's own format
```

- **`.<app>/` is lowercase** ASCII letters, digits and `-`, and is the app's usual name, e.g.
  `.overmind/`. A dot-folder keeps app data out of the way of the profile page, as `.github/` does.
- **An app MUST only read and write inside its own folder.** It MUST NOT touch the root `README.md`,
  which GitHub renders on the user's profile, or another app's folder.
- **An app SHOULD use subfolders per component** when it has several, e.g.
  `.overmind/lane-restart/approvals/`.
- **An app MUST let the user choose the repo and path**, with `<username>/<username>` and `.<app>/` as
  the default suggestion only. Some users keep this data in a private repo instead (§6).

## 3. What an app may treat as the user's data

- **Only content on the repo's default branch counts.** An app MUST resolve the default branch's tip
  commit itself and read at that SHA. GitHub's API serves a fork's commits through the parent repo, so
  an app that accepts any other ref can be shown an unmerged PR's content as if it were real.
- **Failures fail closed.** No repo, no folder, no network or an unparseable file means the app acts as
  though that data does not exist. It MUST NOT fall back to a cached copy it cannot re-verify, or to a
  default that assumes the user's consent.

## 4. Pointers to private repos — `links.json`

```json
{
  "links": [
    {
      "name": "private-approvals",
      "repo": "owner/some-private-repo",
      "path": ".overmind/approvals",
      "purpose": "OverMind approvals that name internal hosts"
    }
  ]
}
```

- `name` identifies the link within this app's folder. `repo` is `owner/name`. `path` is a folder or
  file in it, read at **that** repo's default branch (§3). `purpose` is a human-readable description.
- **The pointer is public, so its values MUST NOT themselves be sensitive.** The repo name, path and
  purpose are visible to everyone.
- An app follows a link with the user's own credentials. If it cannot read the target, that data does
  not exist for it (§3). It MUST NOT follow a link into a repo the user doesn't own unless the user
  configured that link.
- The format borrows its shape from Renovate's `github>owner/repo` presets and `.gitmodules` (§1).

## 5. Only the user can change it

- **Changes arrive as pull requests, and the user's merge is the approval.** An agent or app acting for
  the user SHOULD have **read** access only and propose changes from a fork, so it can never approve on
  the user's behalf.
- **Nobody but the user SHOULD have write access to the repo.** If collaborators are unavoidable, a
  `CODEOWNERS` entry naming only the user for each `.<app>/` path, plus branch protection that requires
  code-owner review, keeps each folder's merges the user's own.
- **A PR SHOULD quote the user's own words, verbatim**, for any decision it records, so the history
  shows what was approved and why.

## 6. No secrets — ever

- **Secrets MUST NOT be stored in these folders or in the repos their links point to**, whether the
  repo is public or private. Secrets include passwords, tokens, keys, PINs, one-time or recovery codes,
  and seed phrases. They belong in a secret store; a git history keeps them forever.
- **A secret is never an approval.** An app MUST NOT let stored data answer a prompt that asks for a
  secret, or type one. `lane-restart` enforces this in code (RESTART-TOOL-DESIGN.md §12.3a).
- **Private by default, public by choice.** An app SHOULD suggest a private repo for anything that would
  reveal more about the user than their public profile does, and SHOULD let the user choose a public one.

## 7. Open questions

- **Licence of this text.** `MIT OR Apache-2.0`, the same as the rest of this repository (see
  `LICENSE-MIT` and `LICENSE-APACHE`), not CC0. The portfolio PM ruled this on 2026-10-25 while
  CireSnave slept (agentlife issue #175); it is the PM's ruling, not CireSnave's own words, and he may
  overrule it.
- **Name and home.** agentlife (ruled 2026-10-25, issue #175). This draft may move to its own repo if
  other apps adopt it.
- **`lane-restart` does not follow `links.json`.** Its config names the approvals repo directly
  (RESTART-TOOL-DESIGN.md §12.2). Checked for #175: `git grep links.json` finds no hit in agentlife
  at `origin/main` 2e8ccb9 outside this file (control: the same tree has 31 lines mentioning `approvals`
  in RESTART-TOOL-DESIGN.md).
