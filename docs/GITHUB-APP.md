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
