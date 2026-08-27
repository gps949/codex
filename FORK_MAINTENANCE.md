# Fork maintenance guide (multi-account Codex)

This fork adds a native multi-account pool (automatic, in-session switching
between several ChatGPT subscriptions) on top of `openai/codex`. This document
describes how to keep the fork in sync with upstream and how to cut releases.

## Branch model

- `main` — pristine mirror of `upstream/main`. Never commit fork code here.
  Update with `git fetch upstream && git merge --ff-only upstream/main`.
- `feature/native-multi-account` — the fork's integration branch. All fork
  functionality lives here as a rebased patch series on top of an upstream
  stable tag.
- `sync/rust-vX.Y.Z` — short-lived branches produced by a sync (rebase of the
  integration branch onto a new upstream stable tag), merged back after tests.

The fork's diff is intentionally mostly _additive_ (new modules such as
`login/src/account_pool.rs`, `core/src/failover*.rs`). The files that modify
upstream code — the "contact surface" — are listed in
`.github/workflows/upstream-sync-check.yml`; conflicts concentrate in
`core/src/session/turn.rs`.

## Sync cadence and procedure

Upstream ships a stable release roughly every 3–4 days and alphas daily.
**Track stable tags (`rust-vX.Y.Z`), not `main`, and sync about once a week.**
Skip alphas unless one carries a fix you need.

### CI on the fork

The inherited upstream CI largely requires self-hosted runner groups, paid
larger macOS runners, BuildBuddy secrets, or bot tokens that only exist in
`openai/codex`. Treat `fork-ci` (rustfmt, clippy `-D warnings` on fork-touched
crates, targeted multi-account tests, codespell, cargo-shear, Prettier) as
this fork's gate.

Note on granularity: `bazel`, `rust-ci`, `sdk`, `repo-checks`, `codespell`,
`cargo-deny`, and `blob-size-policy` are _reusable_ workflows invoked from
inside `blocking-ci` (`on: workflow_call`), so they never appear as separate
entries in the Actions list and cannot be disabled individually — **disabling
`blocking-ci` turns all of them off at once**. GitHub also only registers a
workflow in the Actions list after its trigger has fired at least once, so
never-triggered upstream workflows (issue bots, release pipelines) stay
invisible until something fires them.

Once the fork's integration branch is the default branch, GitHub registers
every workflow file on it. Only self-triggering ones need disabling; the
`workflow_call`-only helpers (Bazel, Codespell, cargo-deny, blob-size-policy,
repo-checks, rust-ci, rust-ci-full-nextest-platform, sdk,
python-runtime-build, publish-r2-release, rust-release-windows,
rust-release-argument-comment-lint) can never run on their own because their
only caller, `blocking-ci`, is disabled.

Disable list (Actions → select workflow → “···” → “Disable workflow”):

- `blocking-ci`, `v8-canary`, `CLA Assistant`
- `postmerge-ci`, `rust-ci-full` — push-triggered heavy CI
- `rust-release`, `rust-release-zsh`, `rusty-v8-release`,
  `python-sdk-release` — tag/push-triggered upstream release pipelines
  (replaced by `fork-release`)
- `rust-release-prepare`, `Close stale contributor PRs` — scheduled
- `Issue Deduplicator`, `Issue Labeler`, `Issue Translator` — need upstream
  bot secrets; fail on every issue event

Keep enabled: `fork-ci`, `upstream-sync-check` (appears after it exists on
the default branch), and the tag-triggered `rust-release*` workflows (they
only run when you push a release tag; trim their target matrix before the
first release). The stale `.github/workflows/native-multi-account-live.yml`
entry is a deleted temporary workflow that can never run again; ignore it.

The `upstream-sync-check` workflow (weekly, metadata-only, ~1 minute) opens an
issue when a new stable tag exists, listing which contact-surface files changed
upstream. Scheduled workflows only run from the repository's **default
branch** — set the default branch to the integration branch (or copy the
workflow there) for the schedule to fire.

Sync steps (merge-based: the integration branch contains PR merge commits, so
rebasing would replay dozens of commits one by one and force-push the default
branch; a merge resolves conflicts once and keeps history append-only):

1. `git fetch upstream --tags`
2. `git checkout -b sync/rust-vX.Y.Z feature/native-multi-account`
3. `git merge rust-vX.Y.Z` — resolve conflicts once (usually only in
   `core/src/session/turn.rs`; the fork's insertions are marked by
   `execution_auth` / `failover` identifiers).
4. Verify:
   - `cargo check -p codex-login -p codex-core -p codex-app-server-protocol -p codex-app-server -p codex-cli`
   - `just test -p codex-login && just test -p codex-config`
   - `just test -p codex-core -E 'test(failover) or test(account_transition) or test(preemptive)'`
   - `cargo clippy --tests -p codex-login -p codex-core -p codex-cli -- -D warnings`
   - Regenerate if protocol/config shapes moved: `just write-config-schema`,
     `python3 codex-rs/app-server-protocol/scripts/write_schema_fixtures.py` (plus `--experimental`)
5. **Auth-format check**: if upstream touched `codex-rs/login/`, confirm the
   per-profile credential homes (`CODEX_HOME/accounts/<id>/auth.json`) still
   load. Upstream migrations only run against the root `CODEX_HOME`; a
   credential format change may need a fork-side migration for profile homes.
6. Update `.github/upstream-baseline.txt` to the new tag, push the sync
   branch, open a PR into `feature/native-multi-account`, merge once
   `fork-ci` is green, and close the sync issue.

### Scheduled Cursor agent (recommended)

Create a weekly scheduled cloud agent with a cheap/Auto model and this prompt:

> Check whether openai/codex has a stable release tag newer than
> `.github/upstream-baseline.txt` on this fork. If not, stop. If yes, create
> `sync/<tag>` from `feature/native-multi-account`, run `git merge <tag>`, and
> follow the verification commands from FORK_MAINTENANCE.md. If the merge is
> conflict-free and tests pass, update `.github/upstream-baseline.txt` to the
> new tag, push the branch, and open a PR titled "sync: <tag>". If there are
> merge conflicts in contact-surface files or test failures, do NOT guess:
> push whatever is safe, then summarize the conflicting hunks and failing
> tests in the PR/issue and explicitly state that a human or a stronger model
> must finish the adaptation.

This keeps the cheap agent on mechanical work and escalates judgment calls.

## Releases

Releases are built by the fork-owned `fork-release` workflow (free standard
runners: Apple Silicon macOS, x64 Linux, x64 Windows). The inherited
`rust-release*` workflows need Apple signing, R2 buckets, and self-hosted
runners — they will fail if they fire on a release tag; disable them in the
Actions UI when they first appear.

- **Tag format**: `rust-vX.Y.Z-ma.N` — upstream baseline plus a fork
  iteration. The binary is stamped `X.Y.Z+ma.N`, and the in-app update check
  compares on the upstream base, so users get an upgrade prompt whenever a
  release moves to a newer baseline.
- **Releasing**: `git tag rust-v0.149.1-ma.1 && git push origin rust-v0.149.1-ma.1`
  from a green integration branch. The workflow builds all three platforms and
  publishes a GitHub release with the archives.
- **Order of operations**: sync + verify first, tag only from a green
  integration branch. Never tag a release from an unsynced/untested state.
- **Install (users)**: download the asset for the platform, extract, put
  `codex` on PATH. No compilation needed. Linux builds link against system
  OpenSSL 3 (any 2022+ distro).

### Coexistence with the official binary (audited)

Verified safe by design — no user action needed:

| Surface                        | Why it is safe                                                                                                                              |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `config.toml` `[account_pool]` | Official builds ignore unknown config sections (only `--strict-config` rejects them)                                                        |
| Root `auth.json`               | The pool never rewrites it; per-profile credentials live under `auth-profiles/<id>/`                                                        |
| OS keyring                     | Entries are keyed by each credential home's path, so profiles and the root login never collide                                              |
| SQLite state DB                | Upstream explicitly tolerates databases migrated by a newer binary running in parallel                                                      |
| Sessions/rollouts              | Official builds ignore the fork's provenance metadata; items they write are genuinely root-account items, so pool attribution stays correct |
| Sibling helper binaries        | Each installation resolves helpers next to its own executable                                                                               |
| Self-update                    | The fork checks its own releases; npm/brew updates of the official build never touch the fork's install dir                                 |
| Daemon socket                  | The fork only reuses a daemon whose version matches; mismatch replaces the managed daemon with this CLI                                     |
| Remote control                 | Enrollment uses a root-only AuthManager; pool rotation never re-pairs the remote identity; daemon spawns this CLI, not packages/standalone  |

Remaining edge cases (documented, not auto-fixable):

- Running the **official** binary with `--strict-config` against a config that
  contains `[account_pool]` errors out; drop the flag or the section.
- An **old official client** (for example a tool-bundled build) may reuse a
  fork daemon it finds on the socket — protocol drift between distant upstream
  versions is upstream's own compatibility domain, not widened by the fork.
- Hook scripts written for older Codex versions may need updating to the
  current hook JSON format regardless of fork vs official.

- **After swapping binaries, restart the shared daemon**: the TUI reuses an
  already-running local app-server daemon socket when one exists. A daemon
  left behind by the official binary (or an older fork build) does not know
  the `accountPool/*` methods, so `/account` fails with "accountPool/read
  failed". Run `codex app-server daemon stop` (fork binary) and relaunch.
- **Remote-control / app-server daemon uses this CLI, not `packages/standalone`**:
  official Codex desktop / `chatgpt.com/codex/install.sh` keep a managed install
  under `~/.codex/packages/standalone/current`. This fork's
  `codex remote-control start` and `codex app-server daemon *` spawn
  **the invoking CLI binary** instead, refuse the official hourly updater, and
  replace a running daemon when its version does not match this CLI. Do not run
  official `app-server daemon bootstrap` beside the fork; it would fight for the
  same control socket.
- **Self-update is intentionally disabled** in this fork: `codex update` and
  the TUI upgrade prompt would otherwise reinstall the official binary over
  the fork. Update checks point at this fork's releases; `codex update` prints
  a pointer to the releases page instead of executing an installer.

## Reset-credit automation

Automatic redemption is **opt-in and rule-bound** because credits are a
limited resource and some users prefer waiting for a nearby natural reset or
saving credits for an account-wide reset event:

```toml
[account_pool]
auto_reset_credits = "when_pool_exhausted" # default: "never"
auto_reset_credit_min_wait_minutes = 60    # skip when the natural reset is closer than this
```

Rules: automation only triggers when **every** pool account is exhausted
(rotating to a free account always wins over spending a credit), and only when
the earliest natural reset is further away than the configured wait threshold.
One credit is redeemed for the account that just failed; the turn then
continues on that account. Failures fall back to the normal pool-exhausted
error. Manual redemption (TUI `/status`, app UI,
`account/rateLimitResetCredit/consume`) is unaffected.

Note: this adds a `codex-backend-client` dependency edge to `codex-core`
(already present in the workspace lockfile). `MODULE.bazel.lock` was not
regenerated because Bazel CI is disabled on this fork; run
`just bazel-lock-update` if Bazel builds are ever re-enabled.

## Fork feature roadmap

All originally planned items are implemented: the `/account` TUI picker,
`accountPool/read|use|updated` app-server APIs, preemptive switching, cooldown
fallback, keep-alive, per-profile re-login and enable/disable, remote-control
identity pinned to the root account (pool rotation does not re-enroll), opt-in
reset-credit automation, and the failover integration test.
