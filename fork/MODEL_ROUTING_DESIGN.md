# Automatic model selection

[简体中文](MODEL_ROUTING_DESIGN.zh-CN.md) · [Account manager](ACCOUNT_MANAGER.md)

Automatic selection chooses a supported model and thinking effort for each **new task** or eligible **new subagent**. It is off by default. Account rotation, tool ranking, skill suggestions and paid API fallback remain separate settings.

## Start in the account manager

Open `codex account manage`, or `codex account manage --tui` and choose **Model selection**. On the phone, send `/account manage` and choose **Model selection** in its native menu.

1. Choose **Preview** to retain the current model and observe suggestions, or **Automatic** to apply compatible choices.
2. Set **Preference**: 0 favors longer use, 100 favors capability. Main tasks and subagents have separate switches.
3. Save. New tasks read the policy; an active task keeps its admitted model.

Start with local rules to avoid classifier fees and sending task text outside Codex. Jev and Clef are alternatives: configure one under **Decision assistance**, then select the configured service in model selection. Tool ranking can remain off. Enabling task classification requires separate consent to share a short task description, even if tool ranking already has consent.

The service receives at most 2048 UTF-8 bytes of task text. It does not receive conversation history, source files, account identities, credentials or quota records. Long or ambiguous tasks keep the current model. A timeout, unavailable service or invalid reply keeps the current choice unless **local fallback** was explicitly enabled. Changing the service destination in the manager disables external model selection until task-sharing consent is confirmed again.

**Local simulation** previews the local policy without sending a service request, saving settings or starting inference. Use **Refresh model catalog** if candidates are missing. This explicitly reads the selected account's model catalog; it does not consume a reset credit or start inference.

## Command line

```sh
# Inspect saved/effective policy.
codex account routing

# Enable local selection, favor longer use, and cap thinking effort.
codex account routing --mode automatic --source local --preference 25 --max-effort high

# Apply selection to subagents only.
codex account routing --main-tasks false --subagents true

# Try a local simulation without saving or calling Jev/Clef.
codex account routing --preview 'Translate this short sentence into English.'

# Use an already configured decision service, with explicit task-sharing consent.
codex account routing --source decision-service --send-task-description true

# Turn automatic selection off.
codex account routing --mode off
```

Use `--allow-model` repeatedly to restrict exact model names, and `--all-models` to return to the supported catalog. `--role MODEL=economy|balanced|capability` assigns your own relative role to an exact name. The browser and terminal manager also provide candidate and role controls.

## How choices are constrained

- Explicit model/effort choices and agent role/default restrictions take priority. Full-history children inherit the parent's choice. Resumed children, steering messages, retries and quota recovery do not trigger another classification.
- Use `/model auto` in the terminal conversation, or **Use auto for this thread** in the phone model menu, to release that conversation's manual model pin. This changes neither the global policy nor task-sharing consent. After this explicit choice, older phone clients' unchanged model snapshots follow the policy; selecting a different model pins it again. API callers can use `modelSelectionIntent: "explicit"` or `thread/settings/update` to pin an unchanged model.
- A candidate must match the current identity's verified catalog or an explicitly configured static catalog. Bundled metadata alone is insufficient. Hidden, specialty, incomplete and unsupported entries are excluded.
- Candidates must fit the retained context, images, supported effort, service tier and child-agent backend. Automatic selection never enables Ultra, changes inference provider, opts into paid API fallback or redeems a credit.
- Local rules require explicit task evidence. Complexity and known risk set a minimum role/effort; ambiguous input retains the current choice. A service may classify complexity but cannot choose arbitrary execution models or grant permissions.
- The shipped exact model profiles provide relative Economy/Balanced/Capability roles. Unknown profiles need an explicit user role. Roles and the 0–100 preference are **not measured subscription consumption multipliers**.

## Limits

Model changes can affect caching, instructions and completion quality. A smaller model is not always cheaper for the whole task if it needs more retries. No real-account benchmark establishes a savings percentage. Inspect actual completion quality and quota use before relying on a more aggressive preference.

A saved policy, a local simulation and an admitted runtime selection are different observations. The manager's latest decision reports only the host that wrote it; it does not prove every connected client adopted the policy or that a task completed successfully. Official phone clients retain their own command-panel behavior; the fork cannot add a new built-in toolbar to a closed-source app.
