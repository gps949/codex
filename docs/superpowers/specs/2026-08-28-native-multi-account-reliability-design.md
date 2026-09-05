# Native Multi-Account Reliability Design

## Purpose

Make the fork's native ChatGPT multi-account pool safe under concurrent turns, account switches,
compaction, process restarts, daemon reuse, and fork releases. The work is limited to behavior and
code introduced or modified by this fork. It does not clean up unrelated upstream Codex code.

## Goals

- Bind every inference request to one immutable execution profile from request construction through
  authentication, transport, provenance, rate-limit attribution, and retry handling.
- Enable account pooling only for first-party ChatGPT subscription execution.
- Preserve portable user and local context while keeping account-scoped server state isolated.
- Make automatic and manual compaction safe for later account transitions.
- Give profile, logout, remote-control, CLI, and app-server operations truthful semantics.
- Prevent cross-process lost updates in account manifests and runtime state.
- Make the fork's CI and release workflow enforce the behavior that the fork depends on.
- Keep fork-owned modules reviewable and public APIs minimal.

## Non-goals

- Changing upstream authentication semantics for stock single-account Codex.
- Refactoring unrelated upstream providers, context management, TUI, app-server, or daemon code.
- Making encrypted server state portable without the source account. An opaque blob cannot be
  decrypted or summarized locally when its owning account is unavailable.
- Adding a general account-management product API to upstream v1 app-server surfaces.

## Core invariants

1. A request's execution profile never changes after its lease is captured.
2. The profile used for network authentication is the profile stamped on resulting history.
3. A failure may mutate availability only for the exact profile and generation that sent it.
4. Local user input and contextual fragments are portable and never assigned to `legacy-root`.
5. Account-scoped opaque data is sent only to its owning profile.
6. The need for history projection depends on persisted history ownership, not current pool size.
7. A configured ChatGPT pool initialization error never silently falls back to unrelated root auth.
8. Custom providers, Bedrock, local providers, and explicit API-key execution ignore the pool.
9. Account-state writes are serialized across processes and cannot overwrite unseen newer state.
10. A release is published only after fork checks pass for the exact source commit.
11. A rate-limit observation never masquerades as an account-identity change.
12. User-facing pool summaries are bounded, prefer labels over internal ids, and are never persisted
    as synthetic conversation history.
13. Duplicate ChatGPT users and shared workspace quota scopes are represented explicitly; profile
    rows are not assumed to be independent quota buckets.

## 1. Request-bound execution authentication

`ExecutionAuthLease` remains the scheduler's immutable identity token, but it also becomes the
request authentication source. The lease exposes the selected profile's `AuthManager` to core.
`run_sampling_request` passes a request binding into `ModelClientSession`; `ModelClient` resolves
ChatGPT auth and API auth from that binding instead of re-reading the mutable compatibility manager.
The same binding remains authoritative across HTTP/WebSocket connection setup and unauthorized
recovery. A 401 may refresh only the bound profile; it cannot resolve a different pool profile and
retry the old prompt underneath the turn loop.

The compatibility manager remains for upstream side systems that are not request scoped. It is not
authoritative for inference, provenance, quota attribution, or reset-credit consumption.

WebSocket and turn-state reuse are tagged with the execution profile id and generation. A mismatch
forces a new session before a request is sent. This covers manual `accountPool/use`, concurrent
failover, preemptive switching, and switching away and back to the same profile.

### Provider gating

Pool execution is eligible only when all of the following are true:

- the provider is the built-in first-party OpenAI provider;
- the provider requires OpenAI authentication;
- no provider-specific API key, bearer token, command auth, Bedrock auth, or workload identity owns
  the request;
- the selected effective authentication is managed ChatGPT subscription authentication.

When the provider is not eligible, core does not install, lease, project history for, or block on
the ChatGPT pool. If the provider is eligible and a manifest exists, initialization failure is a
turn error with an actionable pool diagnostic. Absence of a manifest keeps stock behavior.

## 2. History ownership and projection

History ownership is classified as follows:

- `Portable`: locally authored user input, developer/context fragments, world state, and other
  account-independent harness items. These remain unattributed and keep ids/content annotations.
- `ExecutionScoped(profile, generation)`: model responses and tool results produced during a bound
  request. These use the existing harness metadata sidecar.
- `LegacyRootScoped`: unattributed model/server items from rollouts written before this fork.

The legacy fallback is applied only to model/server item variants, never to every unattributed
item. The projection is always available and decides per item whether sanitization is necessary.
It is used whenever history contains a foreign or legacy scoped item, even if only one profile is
currently configured.

Projection statistics are emitted through tracing/telemetry instead of being discarded. The
encrypted-tool placeholder is represented by a bounded context fragment owned by `core/context`.
Every synthesized `RespondToModel` output is stamped with the request binding.

## 3. Portable compaction

When a pool manifest exists or history contains execution provenance, both automatic and manual
compaction use the local/plaintext compaction path. Before the compaction request is sent, its input
passes through the same ownership projection as normal inference. Compaction output and tool output
are stamped with the request lease.

Size limits:

- retained user/context messages keep the existing aggregate budget but each emitted item is capped
  at 8,000 estimated tokens;
- the compaction summary item is capped at 8,000 estimated tokens;
- no new fork-generated model-visible item may exceed 10,000 tokens;
- projection placeholders remain fixed-size.

### Existing opaque compactions

An opaque compaction remains usable only by its owning profile. When that profile is active and
usable, the next pool-aware compaction converts the thread to a portable plaintext checkpoint.
`accountPool/use` sets a process-wide scheduler preference and cannot inspect every dormant rollout;
therefore each thread preflights its own history before authenticating a request under the preferred
profile. A thread that encounters an unmigrated opaque checkpoint sends no target-authenticated
request and reports the owning profile and the migration command/action.

If the owner is already unavailable, the fork fails closed and preserves the rollout unchanged.
There is no honest way to reconstruct encrypted-only content under another account. This is exposed
as a defined migration-required state, not a generic unsupported-operation failure or silent loss.

## 4. Account lifecycle and persistence

### Identity and quota scopes

Every ready profile resolves a stable ChatGPT user identity and a quota-scope identity from its
stored credentials without exposing either value through logs or UI. Profiles with the same
ChatGPT user identity are historical duplicates: one deterministic canonical profile remains
schedulable and the others remain visible as duplicate/non-schedulable entries until the user
removes them. No credential is deleted automatically.

Distinct Business/Team users may share a workspace quota scope. Workspace-wide depletion updates
every profile in that exact scope, while individual or consumer limits update only the request-bound
profile. Quota scopes are compared by exact internal identity, never merely by grouping plan names
into broad consumer/workspace buckets.

### Transaction model

`account-profiles.json` and `account-runtime-state.json` mutations use one account-pool lock in
`CODEX_HOME`. Each persisted document contains a monotonic revision. Mutation helpers acquire the
lock, load the latest revision, apply one typed change, write a uniquely named temporary file,
flush it, and atomically replace the destination. Runtime-state writers merge their observations
with the latest document rather than replacing unrelated profile changes.

The Windows replacement path uses a recoverable backup/replace sequence so a crash cannot leave the
state missing. Tests run real child processes against one temporary `CODEX_HOME`.

### Command semantics

- `codex account use`: switch a reachable daemon/app-server through `accountPool/use`; otherwise
  persist the next-start selection and say explicitly that no running process was changed.
- `add`, `remove`, `set`, `enable`, and `disable`: use transactional mutations and consistently
  state whether a running process reloaded the result or requires restart.
- removing `legacy-root` is persistent. Runtime startup never recreates a removed entry merely
  because root credentials still exist.
- `codex logout` and app-server `account/logout` clear the entire configured pool plus root auth.
  Single-profile logout/removal remains `codex account remove <profile>`.
- destructive all-account logout reports partial revocation/deletion failures without claiming a
  fully logged-out state.

## 5. Remote Control identity

Remote Control uses a pinned enrollment profile independent from execution rotation. Selection is:

1. an explicitly persisted enrollment profile, if usable;
2. `legacy-root`, if configured and usable;
3. the lowest-priority usable managed profile.

The chosen profile id is persisted so priority edits and normal pool rotation do not silently move
the remote identity. Removing or invalidating the pinned profile requires an explicit replacement
selection and re-enrollment. Same-account token refresh does not reconnect; a real pinned-profile
identity change does.

This permits managed-only installations to use Remote Control without making the active execution
profile control enrollment.

## 6. App-server, CLI, and TUI boundaries

`accountPool/read`, `accountPool/use`, and `accountPool/updated` stay experimental v2 APIs.
`AccountPoolUseParams.profileId` is nullable and optional, `force` is optional with default false,
and all tagged-union fields use camelCase on the wire and in generated TypeScript.

Pool RPC handling and update notification ownership move out of the existing large account
processor into a dedicated fork-owned module. Tests use the public JSON-RPC boundary and
`TestAppServer` auto-environment helpers.

The TUI picker tests cover selection actions, success/error messages, notification-driven status
updates, disabled/auth-broken/cooldown states, and stale-daemon errors in addition to snapshots.

### Typed pool changes and notification discipline

The account pool publishes a typed change describing active identity, availability, rate limits,
profile configuration, or restored cooldown state. App-server emits `account/updated` only when the
effective authentication identity changes. `accountPool/updated` remains the state notification;
rate-limit-only changes never trigger an identity reset, a generic warning, or another unconditional
`account/rateLimits/read` request.

TUI rate-limit threshold state is keyed by execution profile/generation. It resets when that
identity changes, not when any pool snapshot changes. The reset-credit hint is emitted once when an
identity's available count transitions from unknown/zero to positive and is not rediscovered on
every pool update. Updating the rotation strategy synchronizes both `App.config` and the active
`ChatWidget` config before the picker can be reopened.

### Mobile presentation

Mobile compatibility bridges use separate compact and detailed view models instead of reusing one
full pool dump for every surface:

- `/status` is at most six lines and 800 UTF-8 bytes: active display name, aggregate availability,
  and the note that usage belongs to the active profile;
- `/account` lists at most ten profiles and 2,000 UTF-8 bytes, with an overflow count;
- a label is preferred, then email, and an internal profile id is shown only as the final fallback;
- priority and generation are not shown on mobile;
- account-email and rate-limit-name overlays stay single-line and at most 120 UTF-8 bytes;
- generic warnings are reserved for actionable identity/availability transitions and stay under
  240 UTF-8 bytes.

Synthetic mobile slash-command lifecycle events remain ephemeral app-server output. They are not
injected as ordinary user/assistant response items into durable thread history.

## 7. Module and API boundaries

- Split the login scheduler into focused profile/domain, selection/generation, transition, and
  persistence modules, each targeting fewer than 500 production lines.
- Move all newly introduced inline test modules to descriptive sibling `*_tests.rs` files.
- Keep login modules private and re-export only symbols consumed across crate boundaries.
- Replace public positional boolean activation controls with an `AccountActivationMode` enum.
- Extract app-server pool request and notification logic from `account_processor.rs`.
- Do not move unrelated upstream code merely to reduce line counts.

## 8. Fork CI, release, installer, and update

Fork checks are split into reusable jobs and cover every fork-touched Rust crate: login, config,
history, core, CLI, TUI, app-server protocol, app-server, app-server daemon, and app-server
transport. Core and app-server integration tests use auto-environment builders. Linux runs the
full fork behavior suite; Windows runs account-state replacement and CLI smoke tests; release
targets still compile on all supported matrices.

The release workflow invokes the reusable fork gate for its exact SHA before creating/publishing a
release. A tag pushed before checks complete may exist, but publication is refused until that SHA
passes. Workflow dispatch validates before creating its tag.

Release assets include a SHA-256 manifest. `install.sh` downloads to a staging directory, verifies
the selected archive, validates expected sibling binaries, and atomically installs them. It never
kills a PID solely because it appears in a stale file; process identity must match the expected
Codex updater.

`codex update` becomes a fork updater that consumes the same release/checksum contract. It stages
and verifies the new bundle, preserves the current installation until validation succeeds, and
prints the installed fork version. Network and process boundaries are dependency-injected for
tests; no test contacts GitHub.

Release bundles contain every helper the corresponding platform resolves at runtime. Linux embeds
the checksum of the bundled `bwrap` built before the main binary; Windows includes command-runner
and sandbox-setup helpers. Version comparison treats a larger `ma.N` on the same upstream baseline
as an update. The root README identifies the fork and routes installation/update instructions only
to verified fork assets.

## 9. Testing strategy

Every behavior change follows red-green-refactor. Required coverage includes:

- concurrent account selection between lease capture and HTTP/WebSocket send;
- 401 recovery under a bound profile without cross-profile retry;
- stale request failure attribution;
- provider gating for ChatGPT, API key, Bedrock, local, and custom providers;
- malformed manifest fail-closed behavior;
- portable versus legacy ownership projection and single-profile resume;
- automatic/manual compaction before and after account transitions;
- migration-required opaque history behavior;
- partial streaming output, durable output, and tool side-effect reconciliation;
- reset-credit identity, single-consume concurrency, failure, and timeout paths;
- transactional child-process manifest/runtime mutations on Unix and Windows;
- all-account logout and persistent legacy-root removal;
- daemon RPC live selection and next-start fallback;
- Remote Control pinning, managed-only selection, token refresh, and identity replacement;
- app-server public JSON-RPC and notification ordering;
- typed rate-limit versus identity notification behavior and multi-connection fan-out;
- TUI interaction, rotation-current synchronization, rate-limit warning dedupe, and snapshots;
- bounded iOS/Android `/status` and `/account` output without durable-history pollution;
- historical duplicate-user and exact shared-workspace quota behavior;
- installer/update archive verification, atomic replacement, and failure rollback;
- release gate behavior using workflow-level tests or actionlint plus executable helper tests.

## 10. Completion gates

- All fork-specific regression tests pass.
- All changed crate test suites pass.
- Config and app-server schemas are regenerated and clean.
- `just argument-comment-lint` is either run successfully or explicitly left to CI with evidence.
- `just fix -p <crate>` is run for every changed crate, followed by `just fmt` as the final local
  mutation step; tests are not rerun after those commands per repository policy.
- The complete `just test` suite is run only after explicit permission, as required by `AGENTS.md`.
- Independent reviewers find no unresolved critical or important issue in the final diff.
