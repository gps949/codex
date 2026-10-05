# Automatic model selection: proposed design

[简体中文](MODEL_ROUTING_DESIGN.zh-CN.md) · [Decision services](DECISION_MODELS.md)

**Status: design research, not an available feature.** The existing decision advisor ranks deferred tools and optionally suggests skills. It does not currently select Codex models or reasoning effort. The settings below describe the proposed interface, not configuration accepted by this release.

Jev and Clef can classify a task using bounded typed decisions. TypeSafe documents intent/complexity classification followed by application-owned routing; Cloudflare supplies typed-decision models and vendor routing evaluations. Those sources establish technical feasibility, not a measured improvement for this fork's development tasks or subscription limits. [TypeSafe routing](https://docs.typesafe.ai/patterns/intent-routing), [Cloudflare model card](https://huggingface.co/Cloudflare/clef), [Workers AI schema](https://developers.cloudflare.com/workers-ai/models/clef/).

## User controls

The manager would expose one **Automatic model selection** card with independent controls:

| Control             | Proposed behavior                                                                                    |
| ------------------- | ---------------------------------------------------------------------------------------------------- |
| Mode                | **Off** by default; **Preview** records a proposed choice; **Automatic** applies a validated choice. |
| Scope               | Main tasks and subagents have separate switches. Start evaluation with subagents.                    |
| Preference          | Integer **0–100**: 0 prefers longer usage, 50 balances, 100 prefers capability.                      |
| Decision source     | Local rules, or the already configured **Jev / Clef** service. One service is sufficient.            |
| Model/effort limits | An explicit allow-list derived from the current provider's permitted catalog.                        |
| Failure behavior    | Keep the current model and effort. Local fallback can be enabled separately.                         |

The preference is a tradeoff among eligible choices, not a promised quota multiplier. Even at 0, complex work retains a capability floor. At 100, a simple task need not use the strongest model or highest effort. Ultra can change delegation behavior; a slider must never enable it or alter agent permissions implicitly.

Manual model/effort selections, fixed agent roles, administrator restrictions and explicit spawn settings take precedence. Choosing **Automatic** deliberately releases a manual pin. Enabling model selection does not enable tool ranking, skill suggestions, paid inference fallback or automatic credit redemption.

## Decision and application boundaries

1. Build candidates locally from the current provider and authenticated model catalog. Filter supported modalities, context capacity, tool/backend compatibility, effort levels, service tier and access restrictions. Unknown profiles keep the existing valid selection; names alone do not prove cost or capability.
2. Send only the task description and bounded decision metadata to the chosen classifier, never the complete transcript, source files, tool results, account identity or quota records. Ask independent questions about task family, reasoning depth, ambiguity and correctness stakes. Compute the final preference and switching penalty locally.
3. Reject unknown labels, malformed probability distributions, mismatched versions, conflicting assessments and low confidence. Timeout, cancellation, missing credentials, network restrictions or catalog changes retain the current selection. Do not automatically retry a paid classification.
4. Apply through the existing validated Codex settings path. Make one choice at task admission or child-agent creation, then preserve it for steering, retries, continuations and resumed work. Preserve full-history inheritance where the host requires it. Never rewrite history or reset a model client solely to perform routing.

Use the existing decision transport's bounded timeout, concurrency and payload limits, with a separate routing scope and independent consent. Cache keys must include policy, preference, fixed dimensions, catalog revision, provider/credential binding and task boundary. Transport cache lifetime is distinct from a task's stable model choice.

## Why switching every request can waste usage

A weaker model may require more retries or another agent; moving to a smaller context can trigger compaction; changing models can affect caching and model-specific instructions. The policy therefore minimizes total task work rather than choosing the cheapest-looking slug for each request. API prices cannot be converted directly into subscription-window consumption. Without calibrated subscription evidence, show **Usage estimate unavailable** rather than an invented savings percentage.

## Observation and evaluation

Show the current choice, source, supported effort, preference and a short reason such as **Simple bounded edit**, **High ambiguity**, **Context limit** or **Kept manual selection**. Separate **Saved**, **Proposed** and **Applied** states. No generated reasoning transcript is needed.

Implement as reviewable stages: local candidate policy and explanation; mock-tested typed assessment; opt-in child-agent selection; main-task admission and all manager controls. Evaluate completion quality, rework, total tokens, cache behavior, classifier overhead, task latency and actual subscription allowance separately from API charges. No real-provider benchmark has been performed for this proposal.
