# Native Multi-Account Fork Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure the fork tests every owned surface and publishes or installs only verified artifacts built from a passing source commit.

**Architecture:** Factor fork checks into reusable workflow jobs, require them from both branch CI and release, generate a checksum manifest, and share one staged/verified installation contract between `install.sh` and `codex update`.

**Tech Stack:** GitHub Actions, Bash, Rust CLI, SHA-256, actionlint, shellcheck, Bats-style shell fixtures.

**Spec:** `docs/superpowers/specs/2026-08-28-native-multi-account-reliability-design.md`

## Global Constraints

- Never modify repository visibility.
- Never publish from a commit that has not passed the fork gate.
- Installer/update tests use local fixtures and never contact GitHub.
- Existing official updater behavior outside this fork's modified contact surface is out of scope.
- Release artifacts keep sibling helper binaries together.

---

### Task 1: Make fork checks reusable and complete

**Files:**
- Create: `.github/workflows/fork-checks.yml`
- Modify: `.github/workflows/fork-ci.yml`
- Modify: `.github/workflows/fork-release.yml`
- Modify: `FORK_MAINTENANCE.md`

**Interfaces:**
- Produces: reusable `workflow_call` gate accepting the source SHA.
- Covers: login, config, history, core, CLI, TUI, app-server protocol, app-server, daemon, transport.

- [ ] **Step 1: Add executable workflow validation fixtures**

Install/run `actionlint` locally and add a repository script that validates workflow syntax and
ensures release's publish job depends on the reusable fork gate output.

- [ ] **Step 2: Run validation RED**

Expected: current release has no dependency on fork-ci/fork checks.

- [ ] **Step 3: Factor reusable Linux checks**

Split formatting/spelling/schema, Clippy, account/login tests, core integration tests, app-server,
TUI snapshots, and daemon/transport tests into bounded jobs with shared caches.

- [ ] **Step 4: Add Windows account-state/CLI smoke job**

Run the cross-platform account persistence and CLI tests on Windows, not only compilation.

- [ ] **Step 5: Make branch CI and release call the same gate**

Workflow dispatch validates `GITHUB_SHA` before tag creation. Tag-triggered releases check the tag's
peeled commit and refuse publish on gate failure.

- [ ] **Step 6: Run actionlint and commit**

Run: `actionlint .github/workflows/fork-checks.yml .github/workflows/fork-ci.yml .github/workflows/fork-release.yml`

Commit: `ci: gate fork releases on complete checks`

### Task 2: Produce checksummed release bundles

**Files:**
- Modify: `.github/workflows/fork-release.yml`
- Create: `scripts/fork-release/verify-bundle.sh`
- Create: `scripts/fork-release/verify-bundle-tests.sh`
- Modify: `FORK_MAINTENANCE.md`

**Interfaces:**
- Produces: `SHA256SUMS` containing every archive asset exactly once.
- Produces: verifier accepting archive path, checksum manifest, target, and expected sibling list.

- [ ] **Step 1: Write failing verifier tests**

Use temporary archives for valid checksum, mismatch, missing entry, duplicate entry, path traversal,
and missing helper binary cases. Assert literal exit codes and stderr categories.

- [ ] **Step 2: Run RED**

Run: `bash scripts/fork-release/verify-bundle-tests.sh`

Expected: verifier does not exist.

- [ ] **Step 3: Implement strict verification**

Parse only the exact asset basename, reject duplicate/malformed hashes, verify SHA-256 with platform
tools, list archive paths before extraction, and reject absolute/parent-traversal entries.

- [ ] **Step 4: Generate and publish the manifest after all builds**

The release job downloads artifacts, hashes them, signs no unverifiable metadata, uploads
`SHA256SUMS`, and includes the source commit in release notes.

- [ ] **Step 5: Run tests and commit**

Run: `bash scripts/fork-release/verify-bundle-tests.sh`

Commit: `build: checksum fork release bundles`

### Task 3: Make `install.sh` staged, verified, and non-destructive

**Files:**
- Modify: `install.sh`
- Create: `scripts/fork-release/install-tests.sh`

**Interfaces:**
- Consumes: release archive plus `SHA256SUMS`.
- Produces: atomic directory/file replacement with prior binaries preserved until validation.

- [ ] **Step 1: Write failing shell behavior tests**

Stub `uname`, `curl`, `tar`, `command`, `kill`, and Codex daemon output. Cover supported targets,
explicit/latest tags, checksum mismatch, interrupted extraction, `CODEX_INSTALL_NO_PATH`, foreign
daemon, valid updater identity, stale PID, and rollback.

- [ ] **Step 2: Run RED**

Run: `bash scripts/fork-release/install-tests.sh`

Expected: checksum/rollback/stale-PID assertions fail.

- [ ] **Step 3: Download and verify in staging**

Do not write into `INSTALL_DIR` until archive validation and expected-binary checks pass. Move prior
fork binaries to a temporary backup, install all siblings, run `codex --version`, then remove backup.
Restore backup on any failure.

- [ ] **Step 4: Harden daemon/updater cleanup**

Use the fork CLI daemon stop contract. Signal an updater only after process executable and command
line match the expected official updater for the same `CODEX_HOME`.

- [ ] **Step 5: Run shellcheck and tests**

Run: `shellcheck install.sh scripts/fork-release/*.sh`

Run: `bash scripts/fork-release/install-tests.sh`

- [ ] **Step 6: Commit**

Commit: `fix(installer): verify and stage fork upgrades`

### Task 4: Implement a verified fork self-update command

**Files:**
- Create: `codex-rs/cli/src/fork_update.rs`
- Create: `codex-rs/cli/src/fork_update_tests.rs`
- Modify: `codex-rs/cli/src/main.rs`
- Modify: `codex-rs/cli/src/doctor/updates.rs`
- Modify: `codex-rs/cli/Cargo.toml` only for already-workspace dependencies required by the implementation
- Modify: `codex-rs/tui/src/updates.rs`
- Modify: `codex-rs/tui/src/update_prompt.rs`
- Update: relevant TUI update snapshots

**Interfaces:**
- Produces: `ForkUpdateService` with injected release metadata, download, filesystem, and process boundaries.
- Produces: `ForkUpdateOutcome::{AlreadyCurrent, Installed { version }, RestartRequired}`.
- Consumes: the same archive/checksum contract as `install.sh`.

- [ ] **Step 1: Write failing offline updater tests**

Feed local release metadata and archive bytes. Cover current version, newer baseline, newer `ma.N`,
checksum mismatch, wrong target, missing helpers, validation failure, replacement failure, and
rollback. Assert no process/global installation is touched outside the temp fixture.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-cli fork_update`

Expected: current `codex update` always errors.

- [ ] **Step 3: Implement metadata and verification layer**

Reuse the Rust SHA-256/archive safety logic where possible; do not execute downloaded shell. Resolve
the current executable's sibling directory and refuse to replace a package-manager-managed install
whose ownership cannot be established.

- [ ] **Step 4: Implement staged replacement and rollback**

On Unix, use same-filesystem rename. On Windows, stage a small updater/relauncher or return a clear
restart-required result without deleting the running executable. Preserve every prior sibling until
the new bundle validates.

- [ ] **Step 5: Restore TUI update action to the fork service**

Update copy and snapshots so every user-visible instruction points to the fork and remains English.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-cli fork_update`

Run: `just test -p codex-tui update`

Commit: `feat(cli): add verified fork self-update`

### Task 5: Validate release maintenance behavior

**Files:**
- Modify: `.github/workflows/upstream-sync-check.yml`
- Create: `scripts/fork-release/upstream-sync-tests.sh`
- Modify: `FORK_MAINTENANCE.md`

**Interfaces:**
- Preserves: weekly stable-tag radar.
- Produces: complete fork contact-surface inventory derived from the current diff.

- [ ] **Step 1: Write a fixture test for stable-tag selection and contact paths**

Create a temporary Git repository with stable/alpha/prerelease tags and known changed paths. Assert
the script selects the newest stable tag and reports every fork-modified upstream contact surface.

- [ ] **Step 2: Run RED**

Run: `bash scripts/fork-release/upstream-sync-tests.sh`

Expected: inline workflow logic cannot be invoked and the current hard-coded path list misses newer
fork contacts.

- [ ] **Step 3: Extract deterministic helper logic**

Keep issue creation in Actions; move tag selection and contact-path comparison into a local script
used by both tests and workflow.

- [ ] **Step 4: Run validation and commit**

Run: `shellcheck scripts/fork-release/*.sh install.sh`

Run: `bash scripts/fork-release/upstream-sync-tests.sh`

Commit: `test(ci): validate fork sync and release helpers`

### Task 6: Complete platform bundles, version detection, and fork documentation

**Files:**
- Modify: `.github/workflows/fork-release.yml`
- Modify: `codex-rs/tui/src/update_versions.rs`
- Modify: `codex-rs/cli/src/doctor/updates.rs`
- Modify: `codex-rs/linux-sandbox/src/bundled_bwrap.rs`
- Modify: `README.md`
- Modify: `FORK_MAINTENANCE.md`
- Modify: `scripts/fork-release/verify-bundle-tests.sh`

**Interfaces:**
- Preserves: five release targets and fork `X.Y.Z+ma.N` version stamping.
- Requires: every runtime-resolved sibling helper and embedded Linux bwrap digest.

- [ ] **Step 1: Add failing bundle-completeness fixtures**

Assert Linux builds embed the digest of the exact packaged bwrap and Windows bundles contain
`codex-command-runner.exe` plus `codex-windows-sandbox-setup.exe` in addition to existing helpers.

- [ ] **Step 2: Add failing same-baseline update tests**

Assert `0.150.1+ma.1` upgrades to `0.150.1-ma.2`, does not downgrade to `ma.0`, and still upgrades
to a newer upstream baseline. Doctor and TUI must share the same comparison semantics.

- [ ] **Step 3: Run RED and implement release ordering/completeness**

Build, strip, and hash bwrap before compiling Codex with `CODEX_BWRAP_SHA256`. Build/package every
Windows helper resolved by the runtime. Feed the resulting expected-helper list to bundle tests.

- [ ] **Step 4: Correct fork entry documentation**

The root README starts with a clear unofficial-fork notice, supported platforms, verified install
and update commands, and a link to upstream. Remove official install commands that would replace
the fork. Reconcile merge/rebase, workflow enablement, and `auth-profiles/<id>` contradictions in
`FORK_MAINTENANCE.md`.

- [ ] **Step 5: Run validation and commit**

Run: `just test -p codex-tui update_versions`

Run: `just test -p codex-cli doctor::updates`

Run: `bash scripts/fork-release/verify-bundle-tests.sh`

Commit: `fix(release): ship complete fork bundles and updates`
