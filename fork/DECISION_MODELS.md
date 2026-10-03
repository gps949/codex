# Decision models and this fork

[简体中文](DECISION_MODELS.zh-CN.md)

Research checked on 2026-10-03. No live Jev/Clef request, paid benchmark or real-account experiment was performed. The integration below is a proposal, not a shipped acceleration feature.

## What could help

Jev and Clef answer bounded questions using choices, scores and probabilities. They cannot replace the text generation, code editing or tool-argument generation performed by Codex's main model. TypeSafe explicitly distinguishes its coding-agent integration skill from replacing the coding agent's model. [TypeSafe explanation](https://docs.typesafe.ai/introduction/coding-agents)

For this fork, the strongest opportunities are:

| Experiment                       | Proposed benefit                                     | Keep intact                                                     | Evidence required                                                 |
| -------------------------------- | ---------------------------------------------------- | --------------------------------------------------------------- | ----------------------------------------------------------------- |
| Semantic tool/skill ranking      | Find the appropriate tool with fewer failed searches | Full tool accessibility, permissions and stable prompt prefixes | Better relevant-tool recall with lower end-to-end time            |
| Optional reasoning-effort advice | Use an adequate supported effort for simple tasks    | Explicit user effort settings and complex-task quality          | Equivalent task acceptance with less main-model work              |
| Read-only account intent routing | Answer clear account-view requests without inference | Mixed/coding requests; all mutations require explicit actions   | More correctly handled read requests, zero swallowed coding tasks |

These are hypotheses based on the current code paths: `tools/spec_plan.rs`, `tools/router.rs`, `tools/handlers/tool_suggest.rs`, `session/turn.rs`, and the existing host-side mobile account command handlers. There is no existing expensive auxiliary classifier whose replacement automatically saves a model round trip. Adding a classifier and then doing the same main-model work can make the turn slower.

Account ownership, quota arithmetic, reset deadlines, rotation, warmup, credit redemption, idempotency and permissions stay in deterministic code. Jev's own limitation page warns about precise arithmetic, dates, option-order bias and adversarial input. A confidence score is not authorization. [Known limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13)

## Provider boundary and cost

A decision service needs a separate, explicitly enabled configuration. Jev uses `/v1/systemone`; this is not the Responses API expected by the inference account pool. Clef offers a compatible decision payload through Workers AI, with its own authentication and response envelope. Neither belongs in the subscription pool or its paid generative fallback. [TypeSafe API](https://docs.typesafe.ai/api), [Cloudflare model interface](https://developers.cloudflare.com/ai/models/%40cf/cloudflare/clef-flash/)

Current published input prices per million tokens are Jev **$0.042**, Clef-flash **$0.09**, and Clef **$0.24**. For 1,000 requests of 5,000 input tokens each, arithmetic gives **$0.21 / $0.45 / $1.20**, before deployment and other charges. Input includes the state and question definitions. Prices and available limits can change. [Jev pricing](https://docs.typesafe.ai/models), [Cloudflare pricing](https://developers.cloudflare.com/workers-ai/platform/pricing/)

Clef weights are Apache-2.0 and support local experimentation, but ordinary text-generation hosting does not establish support for their specialized decision interface. The 9B and 27B sizes also imply substantial memory and compute needs; local hosting is not automatically a free or faster option. [Clef model card](https://huggingface.co/Cloudflare/clef), [Clef-flash model card](https://huggingface.co/Cloudflare/clef-flash)

## How to establish a real saving

Cloudflare publishes decision-benchmark median latencies of 209.3ms for Clef and 38.8ms for flash. These measure that benchmark, not a full Codex task with network, tool execution, cache behavior and retries. [Cloudflare release evidence](https://developers.cloudflare.com/changelog/post/2026-10-01-clef-workers-ai/)

A useful trial measures total task time, accepted output, main-model requests, reasoning/output tokens, cached input and added decision cost. Keep the original code path as the baseline. Start with synthetic Chinese and English cases, then run live measurements only with a user-selected provider and budget.

For timing, the benefit must exceed decision-call latency plus mistakes and extra retries. For cost, the avoided main-model work must exceed the decision fee and any prompt-cache losses. Less token use is not proof of a specific reduction in ChatGPT subscription-window usage: this fork has no published conversion from tokens to Plus/Business quota percentage.

## Proposed implementation guardrails

- Default off; show the selected decision provider and whether request content leaves the host. Local and hosted providers are distinct choices.
- Use a small, bounded state and the relevant candidate descriptions. Keep credentials and account identifiers out of semantic ranking input.
- Share a bounded cache keyed by actual model version, candidate set and input. Do not repeatedly replace the tool catalog or rewrite conversation history.
- Time out quickly and fall back to the original behavior. Invalid answers, unknown IDs, cancellation and low confidence do not block normal work.
- Never allow the decision output to grant permissions, delete accounts, spend credits or enable paid fallback.
- Begin in an observation mode that cannot change execution. Enable a narrowly scoped behavior only after its own quality and latency comparison passes.

The vendor skill-suggestion example is useful architectural evidence: it adds an advisory suggestion while retaining the original skill roster. Its Hermes/Haiku results are not a Codex coding benchmark. [Skill-suggestion experiment](https://docs.typesafe.ai/cookbooks/skill_suggestion)
