# The runner GitHub App (roadmap C1)

CireSnave, 2026-10-22, verbatim: *"Lets go with the GitHub app.  For that matter, lets lean toward GitHub apps anywhere tokens are required and an app like that can bypass that need more securely."*

An App replaces a personal access token for one job: the AntSpawn watcher (C2) mints a one-hour **installation token** from the
App's private key and uses it to register a **just-in-time runner** for one job (`generate-jitconfig`). Nothing long-lived
and powerful is stored except the App's key, which can mint tokens only for this App's installation.

## What the script does (`scripts/github-app-setup.ps1`)

GitHub's *manifest flow*: the script serves a one-page form on `127.0.0.1`, the browser POSTs the app manifest to
`https://github.com/organizations/<org>/settings/apps/new?state=<one-time>`, GitHub redirects back to the loopback
listener with a single-use `code`, and the script exchanges it (`POST /app-manifests/<code>/conversions`) for the App id and
its private key. The key is stored **protected for the current Windows account** (DPAPI user scope) and never printed.

**CireSnave clicks twice:** (1) "Create GitHub App for ThinkersJournal" on GitHub's page (it lists the permissions: read them);
(2) "Install" on the organization (the script prints the link at the end; it cannot do this click).

```powershell
# as the WATCHER account (the one that will run the AntSpawn watcher), outside any sandbox
pwsh -File scripts\github-app-setup.ps1                 # dry run: prints the manifest, changes nothing
pwsh -File scripts\github-app-setup.ps1 -Apply          # the flow; opens the browser; waits up to 10 min
pwsh -File scripts\github-app-setup.ps1 -Verify         # after click 2: proves key + install + permissions
```

Permissions requested: organization **Self-hosted runners: write**, **Metadata: read**. `-RepoAdminFallback` adds repository
**Administration: write** for `ciresnave`-owned repos that have not moved to the organization yet (roadmap C4); leave it off
once they have. The App is private (installable only on this account), has no webhook, and subscribes to no events.

## What it will and will not do

- **Dry run by default.** `-Apply` refuses when a key already exists in the key directory (a second App would orphan it);
  `-Replace` overrides.
- **Only GitHub, or loopback.** `-GitHubWeb` and `-GitHubApi` accept the official hosts or `http://127.0.0.1|localhost:<port>`
  (that is how the tests stand in for GitHub). Anything else is refused before any request: the code exchange and the JWT go
  nowhere else.
- **The redirect is checked:** one-time 32-byte `state` (constant-time compare), `Host` must be `127.0.0.1:<port>`, `GET` only,
  a code of the expected shape. A wrong or missing state is answered 400 and the script keeps waiting; nothing is exchanged.
- **Secrets:** the key, the client secret and the webhook secret are never printed, logged, put in an argument or an environment
  variable. The client and webhook secrets are discarded (no webhook, no OAuth). Files: `<slug>.key.dpapi` (a DPAPI blob) and
  `<slug>.json` (app id, slug, org, permissions, key path, and after `-Verify` the installation id: not secrets), in a directory
  with an owner-only ACL (`%LOCALAPPDATA%\antspawn\github-app` by default).
- **`-Verify`** signs a JWT (RS256, 9 minutes), asks GitHub for the App and its installations, mints one installation token each,
  checks the permissions on the organization's installation, **revokes the token**, and fails loudly if a permission is missing,
  the signature is refused, or the App is not installed (and then says click 2 is still to do).

## Honest limits

- DPAPI user scope stops a copied disk and other accounts, **not a process running as the watcher account** (the same limit as the
  vault, `docs/DESIGN.md`). That is why the watcher account runs nothing it does not trust, and why the runner account
  (`gh-runner`) never has this directory.
- If GitHub returns the key and writing it fails, the key is lost: create a new one on the App's settings page and rerun with `-Replace`.
- A leaked key can mint installation tokens until it is deleted on the App's settings page. Rotating only the key (generate a new
  one there, store it protected the same way) is not built; `-Apply -Replace` would create a second App, not a new key.
- "The host keystore under the watcher account" is taken to mean this DPAPI directory. If the PM means the `with-secret` vault
  instead, that store is not implemented here (it needs the vault's own approval prompt).
- The manifest's permission key `organization_self_hosted_runners` is the REST documentation's name; the **write** level is
  confirmed only when GitHub's creation page shows it, which is why click 1 is a person reading the list.

## Hand-off to C2 (the AntSpawn adapter)

It needs: `app_id` and `installation_id` (in `<slug>.json` after `-Verify`), the key path, and the JWT/installation-token steps
`-Verify` already performs (`scripts/github-app-setup.ps1` is the reference implementation).

---

# C5: `ciresnave-bot` becomes a GitHub App (design)

CireSnave, 2026-10-22, verbatim: *"Yes on ciresnave-bot as an App."* The runner App (C1/C2, above) comes first. This section is the
design; nothing here is built, and every statement about GitHub's behaviour that this lane has not seen happen is marked
**UNVERIFIED** and is a step of the pilot (section 8), not an assumption.

## 1. What is replaced

`ciresnave-bot` is a GitHub **user** whose `gh` keyring token (scopes `gist, read:org, repo, workflow`) sits on every lane and
on the PM (`docs/TOKEN-INVENTORY.md` section 1). It is long-lived and has `repo` on every repository the user can reach. It is
the largest standing credential we hold. The App replaces it with **installation tokens**: one hour, named repositories, a named
permission subset.

## 2. How a lane gets a 1-hour installation token

The App's private key must never reach a lane: a lane is a Claude session that reads untrusted text. So lanes do not mint; a
**token broker** does.

```
lane (gh / git) --request(lane, repos, tier)--> broker --JWT (RS256, <=10 min)--> POST /app/installations/{id}/access_tokens
                <--token (<=1 h), memory only--          {"repositories": [...], "permissions": {...}}
```

- **The broker** is `agentlife token-broker`, a local service under the watcher account (the account that holds the runner App's
  key in `%LOCALAPPDATA%\antspawn\github-app`, DPAPI user scope). It holds the lane App's key the same way. It listens on a
  loopback named pipe or port and answers one request: *lane X wants repositories R at permission tier T*.
- **Policy lives in the broker**, not in the lane: a table `lane -> allowed repositories and tiers`. A request outside it is
  refused and logged. The token is minted with `repositories` and `permissions` set to the **intersection**, so a lane gets no
  more than the table gives the lane it claims to be (see the limit below).
- **One token has one permission set across all its repositories**, so a lane that needs write on its own repository and read-only
  on `ciresnave/ciresnave` (the approvals repository, on which `ciresnave-bot` has READ today, `RESTART-TOOL-DESIGN.md`) gets
  **two tokens**, not one.
- **The lane side** is a shim, because `gh` reads `GH_TOKEN` once per process and a lane lives for days: a `gh` wrapper earlier on
  `PATH` that asks the broker (cached in memory until 5 minutes before expiry) and runs the real `gh` with `GH_TOKEN` set, and a
  git credential helper for `https://github.com` that does the same (`x-access-token:<token>`). Nothing is written to the `gh`
  keyring, `hosts.yml` or a file. `gh auth status` on a lane then shows no stored login, which is the point.
- **Tiers** (each is a `permissions` object on the token):

  | Tier | Permissions | Used for |
  |---|---|---|
  | `lane` | contents: write, pull_requests: write, issues: write, checks: read, statuses: read, actions: read, metadata: read | push a branch, open and merge a PR, read CI, delete the merged branch |
  | `read` | contents: read, metadata: read | read-only repositories (`ciresnave/ciresnave`) |
  | `workflows` | `lane` plus workflows: write | only when a lane edits `.github/workflows/*`; the bot's `workflow` scope is on every lane all the time today |

  `workflows: write` lets the holder change what CI runs, with that repository's secrets, so it is a **per-request, logged**
  tier, never a default. Administration (branch protection, repository creation) is not in the lane App at all: it stays with the
  PM's separate admin identity (token inventory section 1, `ciresnave`), which becomes its own, later App.
- **Honest limit.** All lanes run as the same Windows user, so the broker cannot prove *which* lane is asking by any OS mechanism
  (lane identity today is a process-tree walk, `RESTART-TOOL-DESIGN.md`: identity cannot come from credentials). A hostile process
  running as the user can ask for another lane's tier. What changes against today: the token it gets dies in an hour, covers only
  the repositories and permissions in the table, is revocable at once (remove the lane's row, or uninstall the App), and every mint
  is in the broker's log with lane, repositories, tier and time. A copied keyring token today is `repo` on everything until someone
  notices. This is a reduction, not a boundary.
- **Rate limit** (documented figure, **UNVERIFIED** for our installations): 5,000 requests per hour per installation. All lanes
  share one installation per owner, so with ~20 lanes polling `gh pr checks` this is the ceiling to watch; the pilot measures it.

## 3. What identity commits and PRs show

- **PRs and comments:** authored by `<slug>[bot]` (REST `user.login`); `gh pr view --json author` shows `app/<slug>` with
  `is_bot` true. **Anything that filters on the author login must change**: `gh pr list --author app/<slug>`, ledgers, scripts.
  In this repository no code reads the bot login (section 5); the PM's ledger and announce trail do (CLAUDE.md section 8).
- **Commits:** a `git push` carries whatever `user.name` and `user.email` the lane configured, and GitHub links the commit to an
  account by **email**. Set the App's noreply address `<app-user-id>+<slug>[bot]@users.noreply.github.com` (the id is
  `GET /users/<slug>%5Bbot%5D`, field `id`) and the commit shows the App's avatar and name. The **author name** is free text, so
  `git log` can say `agentlife lane` while GitHub's UI shows the App. Pushed commits are **unsigned**, as today; commits made
  through the REST contents API are signed by GitHub (**UNVERIFIED** for installation tokens). Trailers (`Co-Authored-By`,
  `Claude-Session`) are unaffected.
- **Merges:** `mergedBy` becomes `<slug>[bot]`. It already attributes nothing in this portfolio (`PORTFOLIO-EVIDENCE.md` item 78:
  `ciresnave-bot` is a lane *or* the PM); one App keeps it that way, per-lane Apps (section 4) fix it.
- **Actions:** pushes and PRs made with an installation token **do** trigger workflows (the `GITHUB_TOKEN` exception does not
  apply). Whether a bot-authored PR from a branch in the same public repository meets the first-time-contributor approval gate is
  **UNVERIFIED** (pilot step).

## 4. Per-lane App option (attribution)

| | One App (`evans-lanes`) | One App per lane (`lane-agentlife`, ...) |
|---|---|---|
| `mergedBy` and author say which lane | no (the lane is only in the author *name* and the broker log) | **yes**, and the PM becomes distinguishable from the lanes |
| Keys to hold | 1 | ~20, each in the broker's DPAPI store |
| Creation clicks | 2 | 2 per lane (the manifest script automates the rest) |
| Revoke one lane | broker table row | delete that App's key or uninstall it, independent of the others |
| Rate limit | one 5,000/h installation shared by all | one per App (**UNVERIFIED**) |
| Per-account limit on Apps owned | none | **UNVERIFIED**: check GitHub's cap before choosing |
| Push-restriction or bypass lists, if any exist (section 5: unread) | one entry | one entry per lane |

**Decided (PM, 2026-10-22): one App first**; per-lane attribution is a later option. Recommendation behind it: start with **one App** and the broker's per-lane log, and move to per-lane Apps only if the PM's need to see
which lane merged what cannot be met from the broker log. The broker's interface does not change between the two (the table maps
a lane to an App id instead of to the one), and the staged move (section 8) works for either.

## 5. What is keyed to the `ciresnave-bot` login (checked 2026-10-22)

| Where | Finding | Consequence |
|---|---|---|
| Repo files (`git grep -i ciresnave-bot` at `origin/main` of every checkout that has a `.git`) | Documentation, changelog prose and one test fixture string (FAM `taken_as: 'ciresnave-bot'`). **No** workflow, script, CODEOWNERS or code path compares against the login. (Control: the same grep does find the doc and test lines.) | Nothing to rewrite in code. The FAM string is a label in a test of a project being decommissioned. |
| CODEOWNERS | 30 of the 113 non-archived repositories under `ciresnave` and `ThinkersJournal` have `.github/CODEOWNERS`, every one `* @ciresnave` (read through the contents API as the bot; the other 83 have none: 68 public, 15 private). | An App cannot be a code owner and need not be. If a rule ever requires a code-owner *review*, App PRs need CireSnave's review exactly as bot PRs do. |
| Rulesets | `0` on every repository where the endpoint answered; the private ones answer an error (rulesets there need a plan this account may not have). | No ruleset bypass list names the bot on a readable repository. |
| Branch protection | The bot is not an admin and can read only `required_status_checks` (`enforcement_level: non_admins`, the two CI legs). `required_pull_request_reviews`, push restrictions and bypass allowances **cannot be read by this lane** (the admin endpoint answers 404 on 90 of the 94 public repositories asked). | **Answered by the PM** (admin read of all 113 repositories, 2026-10-22, file `.archive/protection-bot-refs-2026-10-22.txt`; control: `auth-framework` read OK): 33 repositories protect `main` and **0 mention `ciresnave-bot`** anywhere (no push restrictions, no bypass lists, no required-review allowances). Only `ciresnave/ciresnave` (with `enforce_admins`), `commy-sdk-rust` and `dirty-ranges` require reviews. 61 repositories have no protection (404); 19 answer 403 "Upgrade to GitHub Pro or make this repository public" (private repositories on the free plan, board 108: unprotectable, not a read error). So nothing needs adding for an App except that App PRs to those three need a review, as the bot's do. An App is a *non-admin* like the bot, so required checks apply to it identically. |
| Repository access | `ciresnave-bot` is a collaborator (`write` on `agentlife`, READ on `ciresnave/ciresnave`, 404 on some private repositories). | The App must be **installed on each repository** a lane touches and gets exactly the access given there. Private repositories the bot cannot see today (the `auth-framework-ghsa-*` security forks) are an installation question, **UNVERIFIED**. |
| App owner versus installable owners | The runner App is private and owned by the organization; repositories under the personal account `ciresnave` can be reached only if the App can be installed there. | **UNVERIFIED**, the first pilot question: make the App installable by "any account" (harmless without its key) or use two Apps (one per owner). Moving the OSS repositories to `ThinkersJournal` (C4) removes the problem. |
| Third-party checks (Sourcery, Codacy, Codecov) | Nothing in our repositories says how they treat bot-authored PRs. | **UNVERIFIED**, pilot step: open one App-authored PR and read which analyzers ran. |
| PM tooling | Gates read `mergedBy`, `author` and `gh pr list --head`; `--head` does not depend on the author, `--author` does. | The PM's scripts need `app/<slug>`. Not in this repository. |

## 6. Credentials this removes, and what remains

Removes the `ciresnave-bot` token on every lane's keyring. Does **not** remove: crates.io (Trusted Publishing, token inventory
section 2), the vendor and Cloudflare tokens, or `ciresnave`'s admin token (PM only; later an admin App). The `ciresnave-bot`
**account** stays until the last lane has moved; then its token is revoked on the account's settings page (read back: `gh api user`
with the old token answers 401) and the account is left as an unused collaborator or removed.

## 7. Which lanes build or publish Docker images (the GHCR question)

From `git grep` of `.github/` and `scripts/` at `origin/main` of each checkout, 2026-10-22. Only repositories checked out here are
covered; the `wt-*` and `*-ghsa-*` hits are worktrees or forks of `auth-framework`.

| Repository (lane) | What it does | Registry |
|---|---|---|
| `synapse` | `docker/build-push-action` and `docker/login-action`; secrets `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | **Docker Hub** (a stored token; on GHCR the workflow's own `GITHUB_TOKEN` with `packages: write` does it, token inventory section 3) |
| `auth-framework` | `docker/build-push-action` (two jobs) and `docker build` plus `docker push` to `$ECR_REGISTRY` | **AWS ECR** (the `AWS_*` secrets are not configured, so that job cannot authenticate as written) |
| `auto-discovery` | `cd.yml` with `REGISTRY: ghcr.io`, `docker/login-action` and `build-push-action` | **GHCR already** |
| `PersistAnt` | `docker buildx imagetools inspect mysql:8.4` (reads a digest; publishes nothing) | none |

Publishing to GHCR from CI uses the workflow's `GITHUB_TOKEN` (`packages: write`); the broker is not involved. The GHCR rule is CireSnave's board-249 ruling (PM, 2026-10-22, in the PM archive; its wording is not reproduced here because this lane has not read it). My own search of `CIRESNAVE-EXPECTATIONS.md`, `PORTFOLIO-ROADMAP.md` and `PORTFOLIO-STRATEGY.md` found 0 hits for "ghcr" (those files contain other registry mentions, so the search works): the ruling is not in them.

## 8. Staged rollout (one lane at a time)

0. **Gate:** the runner App (C1/C2) is created and verified first (`github-app-setup.ps1 -Verify`). C5 starts after.
1. **The PM reads** the branch-protection gaps in section 5 and answers the owner question (one App or two).
2. **Build** the broker, the `gh` shim and the credential helper, tested against a stand-in GitHub (the pattern of
   `scripts/tests/github-app-setup.tests.ps1`), each mutation seen red: a request outside the table, an expired token served from
   cache, a token written to disk, a missing `repositories` field.
3. **Pilot on one lane and one repository** (agentlife, `ciresnave/agentlife` only): open one PR with the App token and read back,
   by name, each **UNVERIFIED** item above (author shown, workflows triggered, analyzers run, approval gate, signature,
   `x-ratelimit-limit`). Record the result in this document.
4. **Move one lane at a time**, each with a [NOTICE] and a rollback line (`gh auth login` with the old token keeps working until
   step 6). After each: a merged PR by that lane, `gh auth status` showing no stored login, the broker log showing the mint.
5. **The PM moves last**, to its own tier and App entry, so `mergedBy` can tell it from a lane once per-lane Apps exist.
6. **Revoke** the `ciresnave-bot` token only after every lane and the PM have merged a PR through the broker; read back the 401.

## 9. Not decided here

The App owner and installability (section 5, a pilot question; one App is decided), the admin App, and whether the broker is this repository's
binary or AntSpawn's. Each needs a CireSnave or PM answer before step 2 (the owner question can wait for the pilot).
