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

The inherited upstream workflows (`bazel`, `rust-ci`, `sdk`, `blocking-ci`)
require self-hosted runner groups, paid larger macOS runners, and BuildBuddy
secrets that only exist in `openai/codex` — they can never fully pass here.
Disable them under Actions → (workflow) → “Disable workflow”, and treat
`fork-ci` (rustfmt, clippy `-D warnings` on fork-touched crates, targeted
multi-account tests, codespell, cargo-shear, Prettier) as this fork's gate.

The `upstream-sync-check` workflow (weekly, metadata-only, ~1 minute) opens an
issue when a new stable tag exists, listing which contact-surface files changed
upstream. Scheduled workflows only run from the repository's **default
branch** — set the default branch to the integration branch (or copy the
workflow there) for the schedule to fire.

Sync steps:

1. `git fetch upstream --tags`
2. `git checkout -b sync/rust-vX.Y.Z feature/native-multi-account`
3. `git rebase rust-vX.Y.Z` — resolve conflicts (usually only in
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
6. Force-push the rebased result to `feature/native-multi-account`, update
   `.github/upstream-baseline.txt` to the new tag, close the sync issue.

### Scheduled Cursor agent (recommended)

Create a weekly scheduled cloud agent with a cheap/Auto model and this prompt:

> Check whether openai/codex has a stable release tag newer than
> `.github/upstream-baseline.txt` on this fork. If not, stop. If yes, create
> `sync/<tag>` from `feature/native-multi-account`, rebase onto the tag, and run
> the verification commands from FORK_MAINTENANCE.md. If the rebase applies
> cleanly and tests pass, push the branch and open a PR titled "sync: <tag>".
> If there are conflicts in contact-surface files or test failures, do NOT
> guess: push whatever is safe, then summarize the conflicting hunks and
> failing tests in the PR/issue and explicitly state that a human or a
> stronger model must finish the adaptation.

This keeps the cheap agent on mechanical work and escalates judgment calls.

## Releases

The fork inherits upstream's tag-triggered release pipeline
(`.github/workflows/rust-release.yml`). GitHub Actions on public repositories
are free on standard runners (including macOS), so cost is not a concern —
noise and queue time are; trim the target matrix if you only use one or two
platforms.

- **Version scheme**: `rust-vX.Y.Z-ma.N` — upstream baseline plus a fork
  iteration suffix, so any bug report immediately shows which upstream release
  the build is based on.
- **Order of operations**: sync + verify first, tag only from a green
  integration branch. Never tag a release from an unsynced/untested state.
- **Install**: download the release binary, or `cargo build --release -p
codex-cli` (binary at `codex-rs/target/release/codex`).
- **Self-update is intentionally disabled** in this fork: `codex update` and
  the TUI upgrade prompt would otherwise reinstall the official binary over
  the fork. Update checks point at this fork's releases; `codex update` prints
  a pointer to the releases page instead of executing an installer.

## Intentionally not implemented

- **Automatic reset-credit consumption during failover.** Rotating to another
  pool account is free while a reset credit is a limited resource, so spending
  credits silently is the wrong default. Architecturally it would also force a
  `codex-backend-client` dependency into `codex-core` (upstream explicitly
  resists growing core, and `backend-client` already depends on `codex-login`,
  ruling the login crate out too), adding two lockfiles to the permanent sync
  conflict surface. The pool-exhausted warning instead points users at the
  explicit redeem flow (`account/rateLimitResetCredit/consume`, TUI `/status`).

## Fork feature roadmap (not yet implemented)

- TUI surface: active-account indicator and an `/account` picker. (Automatic
  switches are already visible in every client through warning events.)
- Automatic rate-limit reset-credit consumption before rotating accounts.
- Remote-control enrollment continuity across account switches (enrollment is
  per ChatGPT account id; a rotation currently requires reconnect/re-pair).
