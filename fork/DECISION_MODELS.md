# Decision models and this fork

[简体中文](DECISION_MODELS.zh-CN.md)

Research checked on 2026-10-03. No live Jev/Clef request, paid benchmark or real-account experiment was performed. ma.4 adds an experimental, default-off tool-search advisor. No live speed or subscription-quota savings have been established.

## Set up Jev / Clef in the manager

Run `codex account manage` and select **Choose decision service** under **Decision assistance**. In `codex account manage --tui`, press **D**. Choose Cloudflare Clef, enter your Workers AI Account ID and independent API token, select **Observe only** or **Enabled**, and save. The Clef-flash endpoint is built automatically. TypeSafe Jev uses its own token. Skill hints are a separate opt-in.

Saving sends no service request. **Test connection** sends only a built-in synthetic example, does not save the draft, and may incur an independent service fee. A successful test confirms that request, not task speed or adoption by every running host.

Tokens use separate, endpoint-bound credential storage: they never join the inference pool or appear in config.toml, responses, or browser storage. Changing the provider, service endpoint, or Cloudflare Account ID requires the matching token. Existing environment-variable configurations remain supported; a missing stored token never falls back to another source. Ephemeral credential storage does not survive restarts.

Updated hosts read saved decision settings before subsequent tool searches and root skill suggestions. Existing requests continue, and older running hosts need an upgrade and one restart. Project/session overrides and managed network policy remain in effect. The panel reports saved settings and overrides; execution adoption remains unobserved unless independently confirmed.

To stop assistance, select **Disabled**. Removing a saved token also disables assistance. Advanced options retain compatible custom endpoints, timeouts, and confidence thresholds.

## What could help

Jev and Clef answer bounded questions using choices, scores and probabilities. They cannot replace the text generation, code editing or tool-argument generation performed by Codex's main model. TypeSafe explicitly distinguishes its coding-agent integration skill from replacing the coding agent's model. [TypeSafe explanation](https://docs.typesafe.ai/introduction/coding-agents)

For this fork, the strongest opportunities are:

| Experiment                       | Proposed benefit                                     | Keep intact                                                     | Evidence required                                                 |
| -------------------------------- | ---------------------------------------------------- | --------------------------------------------------------------- | ----------------------------------------------------------------- |
| Semantic tool/skill ranking      | Find the appropriate tool with fewer failed searches | Full tool accessibility, permissions and stable prompt prefixes | Better relevant-tool recall with lower end-to-end time            |
| Optional reasoning-effort advice | Use an adequate supported effort for simple tasks    | Explicit user effort settings and complex-task quality          | Equivalent task acceptance with less main-model work              |
| Read-only account intent routing | Answer clear account-view requests without inference | Mixed/coding requests; all mutations require explicit actions   | More correctly handled read requests, zero swallowed coding tasks |

These are hypotheses based on the current code paths: `tools/spec_plan.rs`, `tools/router.rs`, `tools/handlers/tool_search.rs`, `session/turn.rs`, and the existing host-side mobile account command handlers. There is no existing expensive auxiliary classifier whose replacement automatically saves a model round trip. Adding a classifier and then doing the same main-model work can make the turn slower.

Account ownership, quota arithmetic, reset deadlines, rotation, warmup, credit redemption, idempotency and permissions stay in deterministic code. Jev's own limitation page warns about precise arithmetic, dates, option-order bias and adversarial input. A confidence score is not authorization. [Known limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13)

## Provider boundary and cost

A decision service needs a separate, explicitly enabled configuration. Jev uses `/v1/systemone`; this is not the Responses API expected by the inference account pool. Clef offers a compatible decision payload through Workers AI, with its own authentication and response envelope. Neither belongs in the subscription pool or its paid generative fallback. [TypeSafe API](https://docs.typesafe.ai/api), [Cloudflare model interface](https://developers.cloudflare.com/workers-ai/models/clef-flash/)

Current published input prices per million tokens are Jev **$0.042**, Clef-flash **$0.09**, and Clef **$0.24**. For 1,000 requests of 5,000 input tokens each, arithmetic gives **$0.21 / $0.45 / $1.20**, before deployment and other charges. Input includes the state and question definitions. Prices and available limits can change. [Jev pricing](https://docs.typesafe.ai/models), [Cloudflare pricing](https://developers.cloudflare.com/workers-ai/platform/pricing/)

Clef weights are Apache-2.0 and support local experimentation, but ordinary text-generation hosting does not establish support for their specialized decision interface. The 9B and 27B sizes also imply substantial memory and compute needs; local hosting is not automatically a free or faster option. [Clef model card](https://huggingface.co/Cloudflare/clef), [Clef-flash model card](https://huggingface.co/Cloudflare/clef-flash)

## How to establish a real saving

Cloudflare publishes decision-benchmark median latencies of 209.3ms for Clef and 38.8ms for flash. These measure that benchmark, not a full Codex task with network, tool execution, cache behavior and retries. [Cloudflare release evidence](https://developers.cloudflare.com/changelog/post/2026-10-01-clef-workers-ai/)

A useful trial measures total task time, accepted output, main-model requests, reasoning/output tokens, cached input and added decision cost. Keep the original code path as the baseline. Start with synthetic Chinese and English cases, then run live measurements only with a user-selected provider and budget.

For timing, the benefit must exceed decision-call latency plus mistakes and extra retries. For cost, the avoided main-model work must exceed the decision fee and any prompt-cache losses. Less token use is not proof of a specific reduction in ChatGPT subscription-window usage: this fork has no published conversion from tokens to Plus/Business quota percentage.

## Implemented boundaries

- Default off; show the selected decision provider and whether request content leaves the host. Local and hosted providers are distinct choices.
- Use a small, bounded state and the relevant candidate descriptions. Keep credentials and account identifiers out of semantic ranking input.
- Share a bounded cache keyed by actual model version, candidate set and input. Do not repeatedly replace the tool catalog or rewrite conversation history.
- Time out quickly and fall back to the original behavior. Invalid answers, unknown IDs, cancellation and low confidence do not block normal work.
- Never allow the decision output to grant permissions, delete accounts, spend credits or enable paid fallback.
- Begin in an observation mode that cannot change execution. Enable a narrowly scoped behavior only after its own quality and latency comparison passes.

The vendor skill-suggestion example is useful architectural evidence: it adds an advisory suggestion while retaining the original skill roster. Its Hermes/Haiku results are not a Codex coding benchmark. [Skill-suggestion experiment](https://docs.typesafe.ai/cookbooks/skill_suggestion)

## Use the experimental advisor

The current feature supports **deferred tool-search ranking** and **independently opted-in skill suggestions**. It never automatically loads skills, changes reasoning effort, routes account commands, changes the advertised registry, rewrites existing history, or controls account rotation. `off` performs ordinary BM25 search without extra API calls. `shadow` calls the advisor but returns the same BM25 results; `rank` uses a validated ranking, retaining ordinary results as a fallback. Both enabled modes can incur independent provider charges and send the search query plus tool discovery descriptions to the configured endpoint. Tool metadata can contain workspace information; choose a suitable provider before enabling it.

Start with TypeSafe in observation mode in `CODEX_HOME/config.toml`:

```toml
[decision_advisor]
mode = "shadow"
provider = "typesafe"
model = "jev-1.13.0"
api_key_env = "TYPESAFE_API_KEY"
timeout_ms = 650
min_confidence = 0.35
```

Set the named environment variable in the process that runs Codex. Never put the key itself in config.toml, and do not reuse `OPENAI_API_KEY` or `CODEX_ACCESS_TOKEN`. The TypeSafe endpoint defaults to `https://api.typesafe.ai/v1/systemone`. Use a pinned model version for reproducible comparisons; changing a provider alias can change behavior even when the local configuration stays the same.

For Cloudflare, create a Workers AI token and configure the exact account endpoint:

```toml
[decision_advisor]
mode = "shadow"
provider = "cloudflare"
endpoint = "https://api.cloudflare.com/client/v4/accounts/YOUR_ACCOUNT_ID/ai/run/@cf/cloudflare/clef-flash"
model = "clef-flash"
api_key_env = "CLOUDFLARE_AI_TOKEN"
timeout_ms = 650
```

For the larger model, change both the path suffix and model to `clef`. The adapter uses Workers AI's `result` envelope, not a chat-completions payload. [REST token setup](https://developers.cloudflare.com/workers-ai/get-started/rest-api/)

You can deliberately test with a synthetic catalog before exposing real metadata:

```json
[
  { "name": "source_search", "description": "Find code in repositories" },
  { "name": "weather", "description": "Check the weather forecast" }
]
```

Save it as `decision-tools.json`, then run:

```sh
codex decision-advisor status
codex decision-advisor probe --query "查看明天天气" --catalog decision-tools.json
```

`status` makes no advisor request and shows no secret values. `probe` makes one deliberate call when enabled and reports the validated ranking or fallback reason, plus anonymous counters for that process. The live `tool_search` path uses the same adapters. Debug logs report requests, cache hits, timeouts, accepted rankings, applied changes and fallbacks without query text, candidate text, URLs or credentials. These counters are not an end-to-end benchmark or counters from another daemon process.

After comparing against `mode = "off"`, select `mode = "rank"` if the result quality and total task time improve. Restart the process to load changed configuration; disabling needs only `mode = "off"`.

Requests contain at most 32 candidates, 768 UTF-8 bytes per candidate description and 2,048 query bytes, with a 32 KiB encoded request/response cap. Small catalogs are considered in full, so Chinese or synonym queries can find tools that English lexical search misses. Large catalogs combine lexical candidates with a small deterministic coverage sample; semantic search does **not** guarantee full-catalog recall. The total timeout defaults to 650 ms (configurable 100–1,500 ms), with at most two concurrent calls. There are no automatic paid retries. Cancellation stops waiting and transport work; a provider may already have billed a request it received.

A process-wide cache holds at most 128 entries for 60 seconds, keyed by provider, model, settings, full catalog text revision, candidate set, query and a credential digest. Concurrent identical inputs share one call. Invalid answers and provider failures are retained briefly to avoid rapid repeat requests; every attempt still checks the current application network policy. This service cannot broaden network permission. HTTPS is required for cloud endpoints and redirects are rejected.

For an already deployed local service that implements System One, explicitly use a loopback endpoint such as `http://127.0.0.1:PORT/v1/systemone`, `allow_local_http = true`, and `api_key_env = ""` if that service needs no credential. A normal local text-generation endpoint is insufficient. This fork does not download weights, deploy a model, or start a model server.

## Independently enable skill suggestions

Add `suggest_skills = true` to the existing `[decision_advisor]` section if you want the same service to suggest skills. This defaults to `false`; enabling tool-search ranking does not enable skill suggestions. Begin with `mode = "shadow"` before considering `rank`.

At most one additional request occurs at the start of a root user turn. It contains the latest submitted text in that turn and at most 32 eligible host-discovered skill names/descriptions. It excludes the conversation transcript, full SKILL.md bodies, structured paths and credentials; descriptions themselves may contain sensitive information. Candidates must be discovered, enabled, allow implicit invocation, match the session's product restrictions and have unambiguous names. Explicit skill selections or mentions take precedence and skip the call. Guardian, internal and subagent sessions skip it, as do sessions with skill catalog instructions disabled.

`shadow` records anonymous comparison/latency counters without changing context. `rank` may append one **1,200-byte** advisory context fragment after explicit skill handling, containing one existing skill name and a reminder to follow the user, AGENTS, skill rules and permissions. It does not read a skill body, install dependencies, enable an app or invoke a skill; the main model decides whether to inspect the suggestion through normal skills handling. Existing history and the skill catalog remain unchanged. No match, low confidence, invalid responses, cancellation and timeout add no hint.

This is a working experimental suggestion flow, with no established live quality or speed gain. It currently covers host-discovered skills, not separate cloud or executor skill catalogs. Bounded sampling can miss relevant entries in large catalogs. Suggestions add some main-model context and may add one decision-service charge and wait; compare correct skill selection, rework, total main-model work and complete task time. Set `suggest_skills = false` to disable this scope while retaining tool-search ranking.

## Short service failure cooldowns

Authentication failures, rate limits and server errors temporarily pause new requests for the same endpoint and credential in the current process. Valid `Retry-After` seconds or dates are respected, up to ten minutes. Different tasks share the cooldown; changing the endpoint or credential allows an immediate new attempt. Valid cached results remain available, and current network policy still applies. Model assessment counts, fallbacks, timeouts and latency are tracked separately from tool rankings. These are process-local observations, not subscription savings percentages.
