# Native Multi-Account Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make fork-owned account profiles, runtime state, login/logout, CLI selection, and remote-enrollment identity transactional and truthful across processes.

**Architecture:** Refactor the scheduler into focused private modules, introduce one cross-process account-state transaction lock with monotonic revisions, and expose typed lifecycle operations used by CLI and app-server. Persist Remote Control's enrollment profile separately from the rotating execution profile.

**Tech Stack:** Rust standard file locks, Tokio, tempfile, nextest, Codex CLI integration fixtures.

**Spec:** `docs/superpowers/specs/2026-08-28-native-multi-account-reliability-design.md`

## Global Constraints

- Touch only fork-owned login/account/CLI behavior and the minimum auth-store APIs it consumes.
- Do not move or rewrite unrelated upstream login tests.
- New test modules are sibling `*_tests.rs` files.
- No dependency change unless the Rust standard library and existing workspace utilities cannot implement the transaction safely.
- Credential deletion remains explicit and never follows a non-destructive metadata operation accidentally.

---

### Task 1: Split scheduler modules and narrow the login API

**Files:**
- Modify: `codex-rs/login/src/account_pool.rs`
- Create: `codex-rs/login/src/account_pool/types.rs`
- Create: `codex-rs/login/src/account_pool/selection.rs`
- Create: `codex-rs/login/src/account_pool/transitions.rs`
- Create: `codex-rs/login/src/account_pool_tests.rs`
- Modify: `codex-rs/login/src/lib.rs`

**Interfaces:**
- Produces: `AccountActivationMode::{Normal, ForceQuotaProbe}`.
- Preserves: existing `AccountPool`, lease, snapshot, availability, and rate-limit semantics.
- Removes: public module visibility for account implementation modules; crate-root re-exports remain only where external consumers exist.

- [ ] **Step 1: Run existing scheduler tests before the refactor**

Run: `just test -p codex-login account_pool`

Expected: all existing scheduler tests pass.

- [ ] **Step 2: Move tests to `account_pool_tests.rs` without changing assertions**

Use `#[cfg(test)] #[path = "account_pool_tests.rs"] mod tests;` and rerun the same command.

- [ ] **Step 3: Split types, selection, and transition logic**

Keep `account_pool.rs` as the private facade. Each new production module must remain below 500
lines. Preserve exhaustive availability matches and generation stale-work checks.

- [ ] **Step 4: Replace positional `force: bool` in public account APIs**

Use `AccountActivationMode`; keep wire conversion at app-server/CLI boundaries.

- [ ] **Step 5: Reduce crate exports and compile consumers**

Search every removed export with `rg`; retain only types/functions consumed by core, CLI, or
app-server. Helpers such as credential copying and duplicate reconciliation become `pub(crate)`.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-login account_pool`

Commit: `refactor(login): split account pool internals`

### Task 2: Add cross-process account-state transactions

**Files:**
- Create: `codex-rs/login/src/account_transaction.rs`
- Create: `codex-rs/login/src/account_transaction_tests.rs`
- Modify: `codex-rs/login/src/account_store.rs`
- Create: `codex-rs/login/src/account_store_tests.rs`
- Modify: `codex-rs/login/src/account_runtime_state.rs`
- Create: `codex-rs/login/src/account_runtime_state_tests.rs`
- Modify: `codex-rs/login/src/lib.rs`

**Interfaces:**
- Produces: `AccountStateTransaction::acquire(codex_home) -> io::Result<AccountStateTransaction>`.
- Produces: typed `AccountProfileStore::mutate` and `AccountRuntimeStateStore::mutate` closures.
- Adds: `revision: u64` with `#[serde(default)]` to both versioned wire documents.

- [ ] **Step 1: Write a failing lost-update child-process test**

Spawn two first-party test binaries/processes against one temporary `CODEX_HOME`; pause both after
read, let each add a different profile, and assert the final manifest contains both literal ids.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-login concurrent_profile_mutations_preserve_both_updates`

Expected: one profile is lost under the existing whole-file read-modify-write path.

- [ ] **Step 3: Implement the transaction guard**

Open `.account-pool.lock` with read/write/create, acquire the standard library exclusive lock, and
hold the file for the full read-mutate-flush-replace operation. Return a bounded timeout error only
if a nonblocking caller explicitly requests it; normal lifecycle operations wait for the lock.

- [ ] **Step 4: Replace PID temporary paths with `NamedTempFile`**

Write in the destination directory, flush content, sync the file, persist over the destination, and
sync the parent directory on Unix. Preserve a backup until Windows replacement succeeds.

- [ ] **Step 5: Convert every manifest mutation to the transaction API**

Cover allocation, completion, abandon, legacy-root registration, metadata update, and removal.
Increment revision exactly once per committed mutation.

- [ ] **Step 6: Convert runtime-state writes to merge transactions**

Pool observation writes update only known profile observations and the active profile decision they
own; they preserve newer profiles and revisions read under the lock.

- [ ] **Step 7: Add crash/replacement and runtime lost-update tests**

Assert invalid temporary content never replaces the prior valid state and concurrent profile
removal plus cooldown update preserves the removal.

- [ ] **Step 8: Run tests and commit**

Run: `just test -p codex-login -E 'test(account_store) or test(account_runtime_state) or test(account_transaction)'`

Commit: `fix(login): serialize account state across processes`

### Task 3: Make legacy-root registration and profile reload explicit

**Files:**
- Modify: `codex-rs/login/src/account_runtime.rs`
- Modify: `codex-rs/login/src/account_runtime_tests.rs`
- Modify: `codex-rs/login/src/account_store.rs`
- Modify: `codex-rs/cli/src/account_cmd.rs`
- Create: `codex-rs/cli/tests/account.rs`

**Interfaces:**
- Produces: `AccountPoolRuntime::reload_profiles() -> Result<AccountPoolReload, AccountPoolRuntimeError>`.
- Preserves: root credentials without automatically recreating `legacy-root`.
- Produces: explicit next-start state result for offline CLI mutations.

- [ ] **Step 1: Write failing persistent legacy-root removal test**

Create root ChatGPT auth, register and remove `legacy-root`, reinstall the runtime twice, and assert
the profile remains absent while root `auth.json` remains untouched.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-login removed_legacy_root_is_not_recreated`

Expected: startup re-adds `legacy-root`.

- [ ] **Step 3: Remove implicit runtime registration**

Only explicit pool bootstrap/account-add imports an existing root login. Runtime installation loads
the manifest exactly as persisted.

- [ ] **Step 4: Write failing runtime reload tests**

Add, disable, reprioritize, and remove profiles after runtime installation. Assert reload preserves
valid cooldown/rate-limit observations, removes deleted managers, and reselects deterministically.

- [ ] **Step 5: Implement typed reload and wire command outcomes**

Do not watch files continuously. App-server/daemon callers invoke reload after successful
transactional mutation. Offline CLI commands explicitly report “applies on next start.”

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-login account_runtime`

Run: `just test -p codex-cli --test account`

Commit: `fix(login): make profile reload and root import explicit`

### Task 4: Give CLI account selection truthful live/offline semantics

**Files:**
- Modify: `codex-rs/cli/src/account_cmd.rs`
- Modify: `codex-rs/cli/src/main.rs`
- Modify: `codex-rs/cli/Cargo.toml` only if an existing app-server client dependency is not already available
- Modify: `codex-rs/cli/tests/account.rs`

**Interfaces:**
- Produces: `AccountUseOutcome::{ActivatedRunningProcess, PersistedForNextStart}`.
- Consumes: app-server control socket and experimental `accountPool/use` request.

- [ ] **Step 1: Write failing daemon-live-selection CLI test**

Start a real test app-server on the control socket, run the first-party `codex account use B`
binary through `codex_utils_cargo_bin::cargo_bin`, and assert `accountPool/read` reports B active.

- [ ] **Step 2: Write failing offline-selection output test**

Run the command with no socket. Assert runtime JSON contains B and stderr explicitly says it applies
on next start rather than claiming a running process switched.

- [ ] **Step 3: Run RED**

Run: `just test -p codex-cli --test account account_use`

- [ ] **Step 4: Implement control-socket first, transactional fallback second**

Bound connection attempts; treat version/method mismatch as a clear stale-daemon diagnostic rather
than silently persisting a result that contradicts the running process.

- [ ] **Step 5: Align add/remove/set/enable/disable messages**

Every command reports whether live reload occurred or restart is required. Do not claim credential
deletion if revoke or purge failed.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-cli --test account`

Commit: `fix(cli): apply account selection to running daemons`

### Task 5: Implement all-account logout semantics

**Files:**
- Create: `codex-rs/login/src/account_logout.rs`
- Create: `codex-rs/login/src/account_logout_tests.rs`
- Modify: `codex-rs/login/src/lib.rs`
- Modify: `codex-rs/cli/src/login.rs`
- Modify: `codex-rs/app-server/src/request_processors/account_processor.rs`
- Modify: `codex-rs/cli/tests/account.rs`

**Interfaces:**
- Produces: `logout_all_accounts(config) -> AccountLogoutReport`.
- Produces: report fields for revoked, removed, preserved legacy-root credentials, and failures.
- Consumes: existing `logout_with_revoke` per credential home.

- [ ] **Step 1: Write failing pool logout tests**

Create root plus two managed profiles, execute logout, and assert root auth, managed credentials,
manifest entries, and runtime state are gone. Add a revoke failure and assert the report is partial,
not success.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-login account_logout`

- [ ] **Step 3: Implement transactional logout orchestration**

Snapshot profiles under the lock, revoke/delete each explicit credential home, remove only entries
whose requested cleanup completed, and return every failure. Never recursively delete root.

- [ ] **Step 4: Wire CLI and app-server**

Both legacy logout surfaces use the same operation. CLI output and JSON-RPC success reflect the
complete report; partial failure returns an error containing non-secret profile ids.

- [ ] **Step 5: Run tests and commit**

Run: `just test -p codex-login account_logout`

Run: `just test -p codex-cli --test account logout`

Commit: `fix(login): make logout clear configured account pools`

### Task 6: Persist a stable Remote Control enrollment profile

**Files:**
- Create: `codex-rs/login/src/account_enrollment.rs`
- Create: `codex-rs/login/src/account_enrollment_tests.rs`
- Modify: `codex-rs/login/src/account_store.rs`
- Modify: `codex-rs/login/src/lib.rs`
- Modify: `codex-rs/app-server/src/lib.rs`
- Modify: `codex-rs/app-server-transport/src/transport/remote_control/websocket.rs`
- Modify: `codex-rs/app-server-transport/src/transport/remote_control/websocket_tests.rs`

**Interfaces:**
- Produces: `AccountProfileStore::resolve_enrollment_profile(auth_config) -> Result<Option<AccountProfile>, ...>`.
- Persists: `remote_control_profile_id: Option<AccountProfileId>` in the manifest.
- Consumes: a dedicated profile `AuthManager` in app-server Remote Control setup.

- [ ] **Step 1: Write failing managed-only selection test**

With no root auth and two managed profiles, assert enrollment selects and persists the lowest
priority usable profile. Reorder priorities and assert the persisted selection stays stable.

- [ ] **Step 2: Write failing root-preference and removal tests**

Assert root wins on first selection when configured; removing the pinned profile returns an
explicit replacement-required result rather than silently rotating enrollment.

- [ ] **Step 3: Run RED**

Run: `just test -p codex-login account_enrollment`

- [ ] **Step 4: Implement selection and app-server wiring**

Build Remote Control's manager from the selected profile credential home. Do not install
`AccountPoolExternalAuth` on it.

- [ ] **Step 5: Add transport behavior tests**

Assert execution-pool rotation does not reconnect, same-profile token refresh does not reconnect,
and a real pinned-profile identity change ends the connection for re-enrollment.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-login account_enrollment`

Run: `just test -p codex-app-server-transport remote_control`

Commit: `fix(remote-control): pin enrollment to a usable pool profile`

