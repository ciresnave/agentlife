# Token and credential inventory (roadmap C3), names only

CireSnave, 2026-10-22, verbatim: *"lets lean toward GitHub apps anywhere tokens are required and an app like that can bypass that need more securely."*

**Names, holders, scopes and verdicts only. No value was read or is written here.** Compiled 2026-10-22 by the agentlife lane.

How it was read, and what it does not cover:
- **Vault:** `with-secret vault list` (names, env var, access, lanes, rotate-by). Values never touched.
- **GitHub identities:** `gh auth status` (account names and token *scopes*; the token line was filtered out).
- **CI references:** `git grep` of `secrets.*`, `GH_TOKEN`, `cargo publish`, `id-token` in `.github/` and `scripts/` at each of our checkouts' `origin/main` **after a fresh `git fetch` on 2026-10-22** (four checkouts could not be fetched and are not covered: the two private `auth-framework-ghsa-*` security forks, `claude-code-wrapper`, `llm-bench`; other branches and repos not checked out here are not covered). Repos whose `origin` is not `ciresnave/*` or `ThinkersJournal/*` (for example `cutlass`, NVIDIA's) are not ours and are left out.
- **Configured repo secrets:** `gh api repos/<nwo>/actions/secrets` per repo as `ciresnave-bot` (names only). **Org-level secrets could not be read** (needs `admin:org`; HTTP 403): the PM should list them. Dependabot and environment secrets were not read either.
- **Files that hold a token:** existence and size only (`~/.cargo/credentials.toml`, `~/.npmrc`); not opened.
- **Not inventoried:** Claude Code's own login, claude.ai connectors, Windows Hello, DPAPI keys, `synapsed`'s keystore (a key, not a token).

Not read, so not claimed: whether each `CODECOV_TOKEN` secret is the same value, and whether the vault copy equals them.

Verdicts: **App** = a GitHub App installation token (one hour, per-repo, per-permission) can replace it; **OIDC** = a workflow's short-lived identity token replaces the secret; **No** = not replaceable by either, so it stays a secret (then: rotation and least scope).

## 1. GitHub identities (the largest exposure)

| Identity | Holder | Scopes (as listed) | Verdict |
|---|---|---|---|
| `ciresnave` | PM, per command via `GH_TOKEN="$(gh auth token --user ciresnave)"`, never switched | admin:gpg_key, admin:org, admin:org_hook, admin:public_key, admin:repo_hook, admin:ssh_signing_key, codespace, gist, notifications, project, repo, user, workflow, write:packages | **Mostly App.** As far as the portfolio docs show it is used for: runner registration/removal tokens (App: organization Self-hosted runners, C1/C2) and the admin operations the PM performs (branch protection, repository settings: App Administration permission). **Stays a person's token:** anything done *as CireSnave* (his merge is the approval; `mergedBy: ciresnave`), `write:packages` unless images move to GHCR with `GITHUB_TOKEN`. The scope list is far wider than any one task: an App per task shrinks it. |
| `ciresnave-bot` | every lane (`gh` keyring) | gist, read:org, repo, workflow | **App, but a decision first.** `repo` on every repo the bot can reach, long-lived, on every lane: the biggest standing credential we have. An installation token (one hour, selected repos, only Contents/Pull requests/Checks write) removes that. The cost: commits and PRs show `<app>[bot]` instead of `ciresnave-bot`; any rule, ruleset bypass list, Sourcery/Codacy filter or ledger keyed on the login `ciresnave-bot` changes (portfolio CLAUDE.md s8: `mergedBy` already attributes nothing). Needs its own App, its own board decision (proposed C5). |
| `GITHUB_TOKEN` / `github.token` in workflows | automatic per run | per workflow `permissions:` | Already the right thing. Used for `GH_TOKEN` in HumboldtUnifiedKidTracker, SafeImage, baracuda, commy. Keep `permissions: contents: read` and widen per job. |

## 2. crates.io

| Name | Where | Verdict |
|---|---|---|
| cargo registry token | `~/.cargo/credentials.toml` (one token, 57 bytes), PM publishes locally | **OIDC for CI publishing** (crates.io Trusted Publishing: `permissions: id-token: write` and `rust-lang/crates-io-auth-action`, configured per crate on crates.io, after the first publish). **No** for claiming a new name and for any local publish: those need a token. A crate owner can then forbid token publishing for that crate. |
| `CARGO_REGISTRY_TOKEN` | referenced by `auto-discovery` `cd.yml` (`cargo publish`); **not configured** as a repo secret there (configured: CODECOV_TOKEN only) | The publish step cannot authenticate as written. Either dead workflow or a missing secret: switch it to Trusted Publishing instead of adding the secret. |
| `CRATES_IO_TOKEN` | repo secret on `ciresnave/auth-framework`; **no workflow at that ref references it** | Unreferenced standing publish token: delete it, or move to Trusted Publishing. |

## 3. CI repo secrets (configured, from the API)

| Name | Repos | Verdict |
|---|---|---|
| `CODECOV_TOKEN` | 22 repos: HumboldtUnifiedKidTracker, KISS, OverMind, community, ThinkersJournal.com, ee-amt, aocl, ciresnave, auth-framework, auto-discovery, baracuda, candlelight, coderipper, commy-sdk-rust, commy, FAM, fuel, kiss-ref, lightbulb, mlmf, role-system, vulkane | **OIDC**: `codecov/codecov-action`'s README documents `use_oidc: true` with `permissions: id-token: write`, replacing the upload token. Check the action version in use (README at the pinned SHA) before switching; our workflows set `fail_ci_if_error: false`, so a mistake shows as missing coverage, not red CI. One change per repo, mechanical: a fan-out for a non-Claude provider. (Per Codecov's README as searched 2026-10-22; not tried here.) |
| `DOCKERHUB_TOKEN`, `DOCKERHUB_USERNAME` | `ThinkersJournal/synapse` | **No** at Docker Hub (a personal/org access token). **App/`GITHUB_TOKEN`** if the image moves to GHCR (`write:packages` via the workflow token). A decision, not an upgrade. |
| `SLACK_WEBHOOK_URL` | `ciresnave/auth-framework` | **No** (a webhook URL is the secret). **Name mismatch:** the workflow reads `secrets.SLACK_WEBHOOK`, so the notification step never gets the configured one. |

## 4. Secrets that workflows reference but nothing configures (found by comparing sections 3 and the grep)

| Name | Referenced by | Meaning |
|---|---|---|
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | `auth-framework` `ci-cd.yml` (two `configure-aws-credentials` steps) | Not configured, so those jobs cannot authenticate. **OIDC**: AWS role assumption by the workflow identity needs no stored key. Decide before anyone adds the keys. |
| `PRODUCTION_HOST`, `PRODUCTION_USERNAME`, `PRODUCTION_SSH_KEY`, `STAGING_HOST`, `STAGING_USERNAME`, `STAGING_SSH_KEY` | `synapse` `ci_cd.yml` deploy steps | Not configured. An SSH key is **No** for App/OIDC (short-lived SSH certificates are possible but not built). Do not add them; decide how Synapse is deployed first. |

## 5. The vault (`with-secret vault list`, 18 entries)

Every entry has **`rotate_by=-`: none has a rotation date.** Access is `Write` on all but `TJ_PROD_DATABASE_URL_RO` (`Read`). Lanes: `pm` for all except `GROQ_CLOUD_API_TOKEN` (`overmind`) and `LLAMA_DESKTOP_API_KEY` (`overmind`, `pm`).

| Entries | Verdict |
|---|---|
| `CLOUDFLARE_BUILDS_API_TOKEN`, `CLOUDFLARE_THINKERSFOUNDATION_API_TOKEN`, `CLOUDFLARE_WORKERS_AI_API_TOKEN`, `CACHE_PURGE_TOKEN`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` | **No.** Cloudflare API tokens and R2 S3 keys have no App or workflow-identity form. Least scope per token is the control (check each is scoped to one account/zone/bucket) and a rotation date. |
| `FLY_API_TOKEN` | **No** (Fly deploy tokens are the narrow form: use an app-scoped deploy token, not an org token). |
| `TJ_PROD_DATABASE_URL`, `TJ_PROD_DATABASE_URL_RO` | **No.** A database credential. Prefer the `_RO` one wherever a job only reads; rotation date. |
| `CODECOV_TOKEN` | Section 3: **OIDC** for CI; the vault copy is for manual uploads only. |
| `HUGGINGFACE_API_TOKEN`, `CIVITAI_API_TOKEN`, `GOOGLE_AI_STUDIO_API_TOKEN`, `GROQ_CLOUD_API_TOKEN`, `MISTRAL_API_TOKEN`, `NVIDIA_NIM_API_TOKEN`, `OPENROUTER_API_TOKEN`, `LLAMA_DESKTOP_API_KEY` | **No.** Vendor API keys. They are the cost-routing providers (CIRESNAVE-EXPECTATIONS cost rule): rotation date and per-key spend caps where the vendor offers them. |

## 6. Lane scripts and files

| Item | Verdict |
|---|---|
| Runner registration and removal tokens (`scripts/gh-runner-setup.ps1`, `docs/RUNNER-SETUP.md` s4): fetched per repo by the PM with the `ciresnave` token | **App** (C1/C2): the watcher mints them, or registers JIT runners, from the App key; the PM's admin token leaves the loop. |
| `~/.npmrc` (one entry, 59 bytes; not opened) | Nothing in `.github/` publishes to npm at the refs read. If something does, npm supports trusted publishing from GitHub (**OIDC**); otherwise remove the file. |
| `gh` keyring entries (`ciresnave`, `ciresnave-bot`) | Section 1. |

## 7. Order of work (smallest risk, largest drop first)

1. **C1/C2**: the runner App (script written; two clicks) takes registration tokens off `ciresnave`.
2. **CODECOV_TOKEN to OIDC**, 22 mechanical edits (non-Claude fan-out); then delete the secrets.
3. **crates.io Trusted Publishing** for every crate that already has a published version; then forbid token publishing on it; delete `CRATES_IO_TOKEN` (auth-framework). Local first-publish stays on the cargo token.
4. **Fix or delete the dead references** (auto-discovery `CARGO_REGISTRY_TOKEN`, auth-framework `SLACK_WEBHOOK`/`AWS_*`, synapse `PRODUCTION_*`/`STAGING_*`) so a missing secret stops looking like a working step.
5. **Decide** the `ciresnave-bot` to App move (proposed C5) and the GHCR move for Synapse's image.
6. **Rotation dates** on all 18 vault entries, and a scope check on each Cloudflare token.

## 8. Questions for CireSnave (what I could not read)

1. **Org-level Actions secrets of `ThinkersJournal`** and of the `ciresnave` account: `gh api orgs/ThinkersJournal/actions/secrets` returned HTTP 403 for the lane identity (needs an org admin or the "actions secrets" fine-grained permission). Please list the names (`gh api orgs/ThinkersJournal/actions/secrets --jq '.secrets[].name'` as `ciresnave`). Section 3 covers repo-level secrets only.
2. **Dependabot secrets and environment secrets** (e.g. a `production` environment on `synapse`): not read for the same reason; do any exist?
3. **`~/.npmrc` (one entry) and `~/.cargo/credentials.toml` (one token):** are they still needed? Nothing in CI at the refs read publishes to npm; if nothing local does, the npm entry can go.
4. **The `ciresnave-bot` account's `repo` token on every lane:** do you want it replaced by a GitHub App (proposed C5, section 1)? It changes the login commits and PRs show.
5. **Docker Hub:** is Synapse's image meant to stay on Docker Hub, or move to GHCR so `GITHUB_TOKEN` replaces the Docker Hub secrets?
