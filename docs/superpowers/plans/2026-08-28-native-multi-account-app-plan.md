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
Also cover the non-destructive historical-duplicate state as
`{"type":"duplicateIdentity","canonicalProfileId":"legacy-root"}`.

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

### Task 4: Type account-pool changes and stop notification amplification

**Files:**
- Modify: `codex-rs/login/src/account_pool.rs`
- Modify: `codex-rs/core/src/execution_account_pool.rs`
- Modify: `codex-rs/app-server/src/request_processors/account_pool_processor.rs`
- Modify: `codex-rs/app-server/tests/suite/v2/account_pool.rs`
- Create: `codex-rs/tui/src/app/account_pool_events.rs`
- Create: `codex-rs/tui/src/app/account_pool_events_tests.rs`
- Modify: `codex-rs/tui/src/app/app_server_events.rs`
- Modify: `codex-rs/tui/src/chatwidget/rate_limits.rs`
- Modify: `codex-rs/tui/src/chatwidget/usage.rs`

**Interfaces:**
- Produces: typed pool changes for identity, availability, rate limits, profiles, and cooldowns.
- Preserves: one `accountPool/updated` notification carrying the latest snapshot.
- Restricts: `account/updated`, warnings, and rate-limit refreshes to their semantic triggers.

- [ ] **Step 1: Write failing app-server notification-count tests**

Observe several changed rate-limit snapshots for one active profile. Assert clients receive pool
updates but zero `account/updated` identity notifications and zero generic warnings. Activate a new
profile and assert exactly one ordered identity update plus one pool update.

- [ ] **Step 2: Run RED**

Run: `just test -p codex-app-server account_pool_notification_semantics`

- [ ] **Step 3: Implement typed change propagation**

Do not infer identity changes from arbitrary revision increments. Preserve latest-snapshot delivery
while preventing rate-limit-only observations from entering identity or warning paths.

- [ ] **Step 4: Write failing TUI dedupe tests**

Deliver repeated pool rate-limit updates above 90% with one available reset credit. Assert the 10%
warning and reset-credit hint each appear once. Then switch profile and assert the new identity has
its own warning state and may show one new reset hint.

- [ ] **Step 5: Remove unconditional refresh feedback**

`AccountPoolUpdated` updates the displayed pool snapshot directly. It requests
`account/rateLimits/read` only when the active profile/generation changes and never starts a
startup-reset check for rate-limit-only changes.

- [ ] **Step 6: Run tests and commit**

Run: `just test -p codex-app-server account_pool_notification_semantics`

Run: `just test -p codex-tui -E 'test(account_pool) or test(rate_limit)'`

Commit: `fix(app): separate pool state from account identity`

### Task 5: Make mobile pool status compact and ephemeral

**Files:**
- Modify: `codex-rs/app-server/src/mobile_account_bridge.rs`
- Create: `codex-rs/app-server/src/mobile_account_bridge_tests.rs`
- Modify: `codex-rs/app-server/src/request_processors/account_pool_processor.rs`
- Modify: `codex-rs/app-server/tests/suite/v2/account_pool.rs`

**Interfaces:**
- Produces: bounded compact `/status`, detailed `/account`, single-line overlay, and warning views.
- Removes: durable synthetic status/account response-item injection.

- [ ] **Step 1: Move existing mobile tests to a sibling file**

Run them before and after the move without changing assertions.

- [ ] **Step 2: Write failing literal rendering tests**

Cover labels, email fallback, id-only fallback, long Unicode labels, more than ten profiles, mixed
availability, and cooldowns. Assert exact line/byte limits and that priority/generation/internal ids
are absent when a display name exists.

- [ ] **Step 3: Write failing durable-history test**

Issue mobile `/status` and `/account` through public JSON-RPC. Assert lifecycle notifications are
returned but the thread's persisted response items are unchanged.

- [ ] **Step 4: Implement separate view models and actionable warnings**

Do not reuse a full summary for email, limit name, headline, slash reply, and warning. Rate-limit-only
pool changes emit no warning. Keep every user-visible string English.

- [ ] **Step 5: Run tests and commit**

Run: `just test -p codex-app-server mobile_account_bridge`

Commit: `fix(app-server): compact mobile account status`

### Task 6: Complete TUI account interaction coverage

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

- [ ] **Step 5: Write the rotation-current regression before fixing it**

Select `earliest_reset`, process the successful config-write event, reopen `/account`, and assert
only that strategy is current. Verify both `App.config` and `ChatWidget.config.account_pool` changed.

- [ ] **Step 6: Synchronize the active widget's account-pool config**

Use a focused config-sync method; do not broaden the existing plugin-specific sync helper's name or
responsibility misleadingly.

- [ ] **Step 7: Run TUI tests, review pending snapshots, and accept intended files**

Run: `just test -p codex-tui account_pool`

Run: `cargo insta pending-snapshots -p codex-tui`

Review each `*.snap.new`, then run `cargo insta accept -p codex-tui` only when all pending snapshots
belong to this task.

- [ ] **Step 8: Commit**

Commit: `test(tui): cover account pool interactions`

### Task 7: Test daemon replacement and fork ownership behavior

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
