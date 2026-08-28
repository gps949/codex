# Native Multi-Account App Surfaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make fork-owned app-server, TUI, and daemon account-pool surfaces schema-correct, modular, observable, and behavior-tested.

**Architecture:** Keep the experimental v2 API but move pool handling into a dedicated processor that shares the exact core scheduler. Exercise requests through public JSON-RPC, then wire the TUI and daemon lifecycle to those tested contracts.

**Tech Stack:** Rust, serde, ts-rs, schemars, Tokio, TestAppServer, insta, app-server transport tests.

**Spec:** `docs/superpowers/specs/2026-08-28-native-multi-account-reliability-design.md`

## Global Constraints

- Do not add v1 app-server methods.
- All v2 request optional fields use `#[ts(optional = nullable)]`; wire names are camelCase.
- Use `TestAppServer::builder().build()` and auto-environment thread-start helpers.
- Every user-visible TUI change has snapshot coverage.
- New account-pool logic must not enlarge `account_processor.rs` or `chatwidget.rs`.

---

### Task 1: Correct the experimental v2 schema

**Files:**
- Modify: `codex-rs/app-server-protocol/src/protocol/v2/account_pool.rs`
- Create: `codex-rs/app-server-protocol/src/protocol/v2/account_pool_tests.rs`
- Modify: `codex-rs/app-server-protocol/src/protocol/v2/mod.rs`
- Regenerate: `codex-rs/app-server-protocol/schema/json/**`
- Regenerate: `codex-rs/app-server-protocol/schema/typescript/**`
- Regenerate: `codex-rs/app-server-protocol/schema/precomputed/*.zst`

**Interfaces:**
- Preserves: `accountPool/read`, `accountPool/use`, `accountPool/updated` method names.
- Corrects: optional `profileId`, optional default-false `force`, and `resetsAt` union field.

- [ ] **Step 1: Write failing literal serialization and TS-shape tests**

Assert `{}` deserializes as automatic/non-force selection, serialized default params omit both
fields, explicit null remains accepted, and exhausted availability serializes exactly as
`{"type":"exhausted","resetsAt":123}`.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-app-server-protocol account_pool`

Expected: generated/serialized shape contains required `profileId` and `resets_at`.

- [ ] **Step 3: Apply aligned serde/TS annotations**

Use `#[ts(optional = nullable)]` on `profile_id`, the repository's default-false boolean pattern on
`force`, and explicit aligned serde/TS rename for `resets_at` inside the tagged variant.

- [ ] **Step 4: Regenerate stable and experimental schemas**

Run: `just write-app-server-schema`

Run: `just write-app-server-schema --experimental`

- [ ] **Step 5: Run protocol tests and commit**

Run: `just test -p codex-app-server-protocol`

Commit: `fix(app-server): align account pool wire schema`

### Task 2: Extract the account-pool request processor

**Files:**
- Create: `codex-rs/app-server/src/request_processors/account_pool_processor.rs`
- Create: `codex-rs/app-server/src/request_processors/account_pool_processor_tests.rs`
- Modify: `codex-rs/app-server/src/request_processors/mod.rs`
- Modify: `codex-rs/app-server/src/request_processors/account_processor.rs`
- Modify: `codex-rs/app-server/src/message_processor.rs`
- Modify: `codex-rs/app-server/src/lib.rs`

**Interfaces:**
- Produces: `AccountPoolRequestProcessor` owning read/use RPCs and update task lifetime.
- Consumes: `ExecutionAccountPoolHandle`, `Config`, root/account identity loaders, outgoing sender.
- Leaves: login/logout and upstream account endpoints in `AccountRequestProcessor`.

- [ ] **Step 1: Add characterization tests around existing read/use conversion**

Assert deterministic profile order, active generation, identity fields on read, omitted identity on
notifications, and exact availability/rate-limit wire values.

- [ ] **Step 2: Run tests GREEN before extraction**

Run: `just test -p codex-app-server account_pool_processor`

- [ ] **Step 3: Extract processor and notification owner**

Move only fork-owned pool code. The task aborts when the last processor clone drops. Do not duplicate
pool handles or create a second scheduler.

- [ ] **Step 4: Run characterization tests after extraction**

Run: `just test -p codex-app-server account_pool_processor`

- [ ] **Step 5: Commit**

Commit: `refactor(app-server): isolate account pool processing`

### Task 3: Add public JSON-RPC behavior coverage

**Files:**
- Create: `codex-rs/app-server/tests/suite/v2/account_pool.rs`
- Modify: `codex-rs/app-server/tests/suite/v2/mod.rs`
- Modify: `codex-rs/app-server/src/request_processors/account_pool_processor.rs`

**Interfaces:**
- Consumes: public experimental API through `TestAppServer`.
- Produces: end-to-end contract coverage independent of processor internals.

- [ ] **Step 1: Write failing unconfigured/read/use tests**

Cover `enabled=false`, sorted profile data, specific selection, automatic fill-first, cooldown
rejection, force probe, invalid id, disabled id, and generation increments.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-app-server account_pool`

Expected: missing test setup helpers or mismatched current behavior expose the untested branches.

- [ ] **Step 3: Implement only boundary fixes required by the tests**

Keep errors as JSON-RPC invalid-request/internal-error according to whether input or server state is
invalid. Never expose credential paths or tokens.

- [ ] **Step 4: Add notification ordering and multi-connection tests**

Assert `account/updated` precedes `accountPool/updated`, each subscribed connection receives its own
notification, and dropping one processor stops only its task.

- [ ] **Step 5: Add auto-environment thread test**

Start one thread with `send_thread_start_request_with_auto_env()` and verify account selection does
not alter remote executor OS/environment configuration.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-app-server account_pool`

Commit: `test(app-server): cover account pool JSON-RPC behavior`

### Task 4: Complete TUI account interaction coverage

**Files:**
- Modify: `codex-rs/tui/src/chatwidget/account_popups.rs`
- Create: `codex-rs/tui/src/chatwidget/account_popups_tests.rs`
- Modify: `codex-rs/tui/src/chatwidget/tests/mod.rs`
- Modify: `codex-rs/tui/src/app/app_server_events.rs`
- Create: `codex-rs/tui/src/app/account_pool_events.rs`
- Create: `codex-rs/tui/src/app/account_pool_events_tests.rs`
- Modify: `codex-rs/tui/src/app/mod.rs`
- Update: `codex-rs/tui/src/chatwidget/snapshots/*account_pool*.snap`

**Interfaces:**
- Preserves: `/account` command and existing `AppEvent` request flow.
- Produces: tested mapping for automatic/profile selection and notification-driven status updates.

- [ ] **Step 1: Move account popup tests to the sibling file**

Run the existing snapshot before and after the move.

- [ ] **Step 2: Write failing selection-action tests**

Invoke the real selection action and assert automatic sends `profile_id: None`, profile selection
sends its literal id, and disabled/auth-broken rows do not offer an unsafe normal activation.

- [ ] **Step 3: Add UI state snapshots**

Cover available, active, disabled, auth-broken, cooldown with UTC time, read error, use error, and
successful switch. All copy remains English.

- [ ] **Step 4: Extract notification handling and test it**

Assert pool notifications update the `/status` identity after `account/updated`, clear stale rate
limits, and initiate exactly one refresh without treating it as startup reset-credit discovery.

- [ ] **Step 5: Run TUI tests, review pending snapshots, and accept intended files**

Run: `just test -p codex-tui account_pool`

Run: `cargo insta pending-snapshots -p codex-tui`

Review each `*.snap.new`, then run `cargo insta accept -p codex-tui` only when all pending snapshots
belong to this task.

- [ ] **Step 6: Commit**

Commit: `test(tui): cover account pool interactions`

### Task 5: Test daemon replacement and fork ownership behavior

**Files:**
- Modify: `codex-rs/app-server-daemon/src/lib.rs`
- Create: `codex-rs/app-server-daemon/src/lifecycle_tests.rs`
- Modify: `codex-rs/app-server-daemon/src/managed_install_tests.rs`
- Modify: `codex-rs/app-server-daemon/src/update_loop.rs`
- Modify: `codex-rs/app-server-daemon/src/update_loop_tests.rs`
- Modify: `codex-rs/tui/src/lib.rs`
- Create: `codex-rs/tui/src/app_server_daemon_tests.rs`

**Interfaces:**
- Preserves: current-CLI daemon ownership and refusal to run the official standalone updater.
- Produces: executable identity check for updater PID/process cleanup.

- [ ] **Step 1: Write lifecycle integration tests**

Use temporary sockets and first-party test binaries to cover same-version reuse, managed version
mismatch replacement, foreign app-server refusal, and missing/not-ready managed backends.

- [ ] **Step 2: Run RED for untested lifecycle branches**

Run: `just test -p codex-app-server-daemon lifecycle`

- [ ] **Step 3: Add updater process-identity verification**

Before signaling a PID from the updater pid file, verify it belongs to the expected Codex updater
binary and expected `CODEX_HOME`. A stale/reused PID is removed from state without being signaled.

- [ ] **Step 4: Test TUI embedded fallback policy**

Cover same-version daemon reuse, version mismatch replacement, connection failure fallback, and the
explicit-remote path that must not silently fall back to embedded.

- [ ] **Step 5: Run tests and commit**

Run: `just test -p codex-app-server-daemon`

Run: `just test -p codex-tui app_server_daemon`

Commit: `test(daemon): verify fork app-server ownership`

