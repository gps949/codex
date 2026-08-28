# Native Multi-Account Core Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every fork-owned multi-account inference, history, failover, and compaction path use one immutable execution identity safely.

**Architecture:** Add an explicit request auth binding derived from `ExecutionAuthLease`, gate pooling to eligible first-party ChatGPT subscription providers, classify history ownership per item, and route all pool-aware sampling and compaction through one projection path. The compatibility `AuthManager` remains for upstream side systems but cannot authenticate inference.

**Tech Stack:** Rust, Tokio, wiremock, Codex core integration fixtures, nextest.

**Spec:** `docs/superpowers/specs/2026-08-28-native-multi-account-reliability-design.md`

## Global Constraints

- Modify only fork-introduced multi-account behavior or the smallest upstream contact surface needed to call it.
- Do not modify sandbox environment-variable behavior.
- Use `TestCodexBuilder::build_with_auto_env()` for new core integration coverage.
- Put new test modules in sibling `*_tests.rs` files.
- No model-visible item may exceed 10,000 tokens; fork-generated items target 8,000 tokens.
- Use self-documenting enums instead of positional boolean controls.

---

### Task 1: Provider eligibility and fail-closed initialization

**Files:**
- Modify: `codex-rs/core/src/execution_auth.rs`
- Modify: `codex-rs/core/src/session/turn.rs`
- Create: `codex-rs/core/src/execution_auth_tests.rs`
- Modify: `codex-rs/core/src/lib.rs`

**Interfaces:**
- Produces: `ExecutionAuthMode::{Stock, Pooled(Arc<AccountPoolRuntime>)}`.
- Produces: `ExecutionAuth::mode_for_turn(&Config, &ModelProviderInfo) -> Result<ExecutionAuthMode, AccountPoolRuntimeError>`.
- Consumes: resolved provider and auth configuration already present in `TurnContext`.

- [ ] **Step 1: Write failing provider-gating tests**

Add table-driven tests proving that only the built-in OpenAI provider with managed ChatGPT auth is
pool eligible. Literal cases must cover ChatGPT, API key, Bedrock, custom `requires_openai_auth =
false`, custom bearer auth, and workload identity.

```rust
assert_eq!(pool_eligibility(&chatgpt_provider, AuthMode::Chatgpt), PoolEligibility::Eligible);
assert_eq!(pool_eligibility(&ollama_provider, AuthMode::Chatgpt), PoolEligibility::Ineligible);
assert_eq!(pool_eligibility(&openai_provider, AuthMode::ApiKey), PoolEligibility::Ineligible);
```

- [ ] **Step 2: Run tests and verify RED**

Run: `just test -p codex-core execution_auth`

Expected: compilation failure because `pool_eligibility` and `ExecutionAuthMode` do not exist.

- [ ] **Step 3: Implement the pure eligibility predicate and mode resolution**

Keep provider policy in core, not login. `mode_for_turn` must return `Stock` without probing the
manifest for ineligible providers. For eligible providers, absence of a manifest returns `Stock`;
an invalid configured pool returns its exact initialization error.

- [ ] **Step 4: Add a failing integration test for malformed-manifest fail-closed behavior**

Create a temporary `CODEX_HOME/account-profiles.json` containing invalid JSON, start a ChatGPT turn,
and assert no `/responses` request is sent and the turn reports the pool initialization error.

- [ ] **Step 5: Run the integration test RED, then wire `run_turn` to `mode_for_turn`**

Run: `just test -p codex-core malformed_account_pool`

Expected before implementation: the stock request is observed. After implementation: the test
passes and the stock auth path is not used.

- [ ] **Step 6: Run scoped tests and commit**

Run: `just test -p codex-core execution_auth`

Commit: `fix(core): gate account pooling to subscription execution`

### Task 2: Bind request authentication to the execution lease

**Files:**
- Modify: `codex-rs/core/src/execution_auth.rs`
- Modify: `codex-rs/core/src/client.rs`
- Modify: `codex-rs/core/src/session/turn.rs`
- Create: `codex-rs/core/src/execution_request_auth.rs`
- Create: `codex-rs/core/src/execution_request_auth_tests.rs`
- Modify: `codex-rs/core/tests/suite/account_failover.rs`

**Interfaces:**
- Produces: `ExecutionRequestAuth { profile_id: Option<AccountProfileId>, generation: u64, auth_manager: Arc<AuthManager> }`.
- Produces: `ExecutionAuthLease::request_auth() -> ExecutionRequestAuth`.
- Consumes: `ExecutionRequestAuth` in every Responses HTTP/WebSocket setup path.

- [ ] **Step 1: Write a failing concurrent-switch integration test**

Use two profile auth fixtures. Pause request A after lease capture, activate B through the shared
pool, release A, and assert the first HTTP request still carries A's bearer token while the next
request carries B's token. Assert persisted provenance matches each token.

- [ ] **Step 2: Run the concurrent test and verify RED**

Run: `just test -p codex-core request_auth_stays_bound_after_concurrent_switch`

Expected: request A carries B's authorization under the existing mutable manager path.

- [ ] **Step 3: Add `ExecutionRequestAuth` and thread it into `ModelClientSession`**

The binding is immutable and request-scoped. Refactor `current_client_setup` to accept an optional
binding; when present, resolve ChatGPT auth/API auth from the bound profile manager. Stock callers
continue passing no binding.

- [ ] **Step 4: Tag WebSocket session reuse with profile and generation**

Add an exhaustive identity comparison before incremental WebSocket reuse. A different profile or
generation clears the connection, last request, response receiver, and turn-state token.

- [ ] **Step 5: Run HTTP and WebSocket tests GREEN**

Run: `just test -p codex-core request_auth_stays_bound`

Run: `just test -p codex-core websocket_execution_identity`

- [ ] **Step 6: Add stale-failure attribution coverage**

Complete B after A was switched out, then deliver A's late 429. Assert B remains active and only A's
availability changes when A was still the request owner.

- [ ] **Step 7: Run scoped tests and commit**

Run: `just test -p codex-core -E 'test(account_failover) or test(execution_request_auth)'`

Commit: `fix(core): bind inference requests to account leases`

### Task 3: Correct portable and legacy history ownership

**Files:**
- Modify: `codex-rs/core/src/account_transition.rs`
- Create: `codex-rs/core/src/account_transition_tests.rs`
- Modify: `codex-rs/core/src/session/turn.rs`
- Modify: `codex-rs/core/src/execution_provenance.rs`
- Modify: `codex-rs/core/src/stream_events_utils.rs`
- Create: `codex-rs/core/src/context/account_transition_notice.rs`
- Modify: `codex-rs/core/src/context/mod.rs`

**Interfaces:**
- Produces: `HistoryItemOwnership::{Portable, ExecutionScoped, LegacyRootScoped}`.
- Produces: `AccountHistoryTransition::history_requires_projection(&[ResponseItemEnvelope]) -> bool`.
- Produces: bounded `AccountTransitionToolOutputNotice` implementing `ContextualUserFragment`.

- [ ] **Step 1: Move existing account-transition tests to a sibling file**

Run them unchanged before moving and after moving to establish refactor safety.

- [ ] **Step 2: Write failing portable-context tests**

Construct unattributed user, developer context, world-state message, assistant, reasoning, tool
call/output, and compaction items. Assert only model/server variants become legacy-root scoped;
portable items retain response ids and internal content annotations under a managed target.

- [ ] **Step 3: Run RED**

Run: `just test -p codex-core portable_unattributed_history`

Expected: the current blanket legacy-root fallback strips metadata from portable items.

- [ ] **Step 4: Implement exhaustive ownership classification**

Match every `ResponseItem` variant. Keep user/developer/context fragments portable. Treat legacy
assistant/reasoning/tool/server compaction items as root-scoped. Do not add a wildcard match arm.

- [ ] **Step 5: Write and fix the single-profile foreign-history regression**

Resume history containing profile A model state with only profile B configured. Assert projection
still strips A's opaque account state rather than bypassing filtering because pool length is one.

- [ ] **Step 6: Fix synthesized output provenance and context abstraction**

Route `RespondToModel` outputs through `record_conversation_items_with_execution_provenance` and
replace the raw encrypted-output string with `AccountTransitionToolOutputNotice`.

- [ ] **Step 7: Emit projection statistics**

Record nonzero counts in tracing with target profile and generation, without profile credentials or
OAuth account identifiers.

- [ ] **Step 8: Run scoped tests and commit**

Run: `just test -p codex-core account_transition`

Commit: `fix(core): preserve portable context across account switches`

### Task 4: Make all pool-aware compaction portable and bounded

**Files:**
- Modify: `codex-rs/core/src/compact.rs`
- Modify: `codex-rs/core/src/portable_compaction.rs`
- Modify: `codex-rs/core/src/tasks/compact.rs`
- Modify: `codex-rs/core/src/session/turn.rs`
- Create: `codex-rs/core/src/portable_compaction_tests.rs`
- Modify: `codex-rs/core/tests/suite/account_failover.rs`

**Interfaces:**
- Produces: `PortableCompactionPolicy::for_history(execution_mode, history)`.
- Produces: `project_history_for_execution(...)` shared by normal sampling and compaction.
- Produces: `MAX_PORTABLE_CONTEXT_ITEM_TOKENS = 8_000`.

- [ ] **Step 1: Write failing automatic-compaction projection test**

Build history with profile A encrypted reasoning and switch to B before automatic compaction. Assert
the compaction request contains the readable summary but not A's encrypted blob, ids, or routing
metadata.

- [ ] **Step 2: Write failing manual `/compact` regression**

With B active and remote compaction supported, issue `Op::Compact`. Assert the request uses local
compaction and the following normal turn succeeds under B.

- [ ] **Step 3: Run both tests RED**

Run: `just test -p codex-core pool_compaction`

Expected: automatic local compaction sends raw history and manual compaction selects remote output.

- [ ] **Step 4: Extract and use one history projection helper**

Both `run_sampling_request` and local compaction call the helper with the same lease and annotated
history. Compaction output uses execution provenance recording.

- [ ] **Step 5: Apply portable policy to manual and automatic compaction**

Pool-configured or provenance-bearing history always selects local/plaintext compaction. Stock
single-account history retains upstream remote-compaction behavior.

- [ ] **Step 6: Write failing item-size tests**

Use a 20,000-token user message and an oversized model summary. Assert every resulting response
item estimates to at most 8,000 tokens and recent content is retained.

- [ ] **Step 7: Implement independent per-item and summary caps**

Derive expectations with literal lengths. Preserve the aggregate retained-user budget while
truncating each item separately.

- [ ] **Step 8: Run scoped tests and commit**

Run: `just test -p codex-core -E 'test(compact) or test(account_transition)'`

Commit: `fix(core): make account-pool compaction portable`

### Task 5: Define opaque-history migration and request-bound transition preflight

**Files:**
- Modify: `codex-rs/core/src/account_transition.rs`
- Modify: `codex-rs/core/src/failover.rs`
- Modify: `codex-rs/core/src/failover_turn.rs`
- Modify: `codex-rs/core/src/session/turn.rs`
- Create: `codex-rs/core/src/opaque_history_migration.rs`
- Create: `codex-rs/core/src/opaque_history_migration_tests.rs`
- Modify: `codex-rs/core/tests/suite/account_failover.rs`

**Interfaces:**
- Produces: `AccountTransitionReadiness::{Ready, MigrationRequired { owner_profile_id }}`.
- Produces: `preflight_account_transition(history, target_profile) -> AccountTransitionReadiness`.
- Produces: an actionable protocol warning/error that preserves the original history.

- [ ] **Step 1: Write failing user-directed-switch request-preflight test**

Create a legacy opaque compaction owned by root and attempt to select B. Assert the active profile
preference changes, but the next turn sends no B-authenticated request and the error names root as
the required migration owner.

- [ ] **Step 2: Write failing automatic-failover preflight test**

Have A return 429 while history contains an A opaque checkpoint. Assert the scheduler records A's
cooldown but sends no B-authenticated request and never sends A's blob to B.

- [ ] **Step 3: Run RED**

Run: `just test -p codex-core opaque_history_transition`

- [ ] **Step 4: Implement preflight before target-authenticated request construction**

Evaluate target readiness using cloned annotated history after scheduler selection but before
building client auth or transport. Return the typed migration-required state without opening a
target-authenticated HTTP or WebSocket connection.

- [ ] **Step 5: Add owner-active migration coverage**

When the owner remains usable, run local compaction under that owner, persist a portable summary,
then allow selection of B. Assert the original rollout remains recoverable if migration fails.

- [ ] **Step 6: Run integration tests and commit**

Run: `just test -p codex-core -E 'test(opaque_history) or test(account_failover)'`

Commit: `fix(core): preflight opaque history before account changes`

### Task 6: Complete failover, tool, and reset-credit integration coverage

**Files:**
- Modify: `codex-rs/core/tests/suite/account_failover.rs`
- Modify: `codex-rs/core/src/reset_credit_rescue.rs`
- Modify: `codex-rs/core/src/reset_credit_rescue_tests.rs`

**Interfaces:**
- Consumes: request-bound auth and portable-history APIs from Tasks 1-5.
- Produces: behavior-level coverage for all fork-owned retry modes.

- [ ] **Step 1: Convert the existing fixture to `build_with_auto_env()`**

Run the existing 429 test before and after conversion.

- [ ] **Step 2: Add partial-output, durable-output, and tool-side-effect tests**

Assert partial visible output requires resend, a completed assistant item continues from durable
history, and a tool executes once when the following sampling request fails.

- [ ] **Step 3: Add quota/auth/pool-exhaustion cases**

Cover `QuotaExceeded`, permanent refresh-token failure, stale failure, and no eligible target.

- [ ] **Step 4: Add reset-credit API tests**

Mount the real backend route and assert the failed profile's authorization is used, exactly one
consume request is made under concurrent turns, default/near-reset cases make zero requests, and
failure/timeout never reports success.

- [ ] **Step 5: Run all fork core tests and commit**

Run: `just test -p codex-core -E 'test(account_failover) or test(account_transition) or test(preemptive) or test(reset_credit)'`

Commit: `test(core): cover multi-account recovery boundaries`
