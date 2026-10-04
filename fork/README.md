# Multi-account Codex: user guide

[简体中文](README.zh-CN.md) · [Releases](https://github.com/gps949/codex/releases) · [Maintenance](../FORK_MAINTENANCE.md)

This fork keeps several ChatGPT accounts or Business seats in one local pool. Codex selects an available account, switches when quota runs low, and can wait for quota to recover without making you log out and back in. Each profile belongs to a specific ChatGPT user and workspace; the same email can have separate personal and Business profiles.

## Start with two accounts

Install on Apple Silicon macOS or Linux x64/arm64:

```sh
curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash
codex --version
```

The version includes `+ma.N`. Linux bundles require system OpenSSL 3. For Windows x64/arm64, download the matching ZIP from [Releases](https://github.com/gps949/codex/releases), extract it, and add its folder to PATH. Keep every bundled helper beside `codex` or `codex.exe`.

Enroll accounts with recognizable, unique labels:

```sh
codex account add --label "Personal"
codex account add --label "Work"
codex account list
codex
```

Select the intended account and workspace during each login. Adding the same user and workspace again refreshes its existing profile. The pool no longer imports an existing root login when it is created. On a host without a browser, use `codex account add --label "Work" --device-auth`.

Labels are optional: `codex account add` displays the account's email after login, falling back to its profile ID if no email is available. Set a custom label to distinguish personal and Business profiles with the same email. Check the resulting names with `account list`; `codex account set "Work" --clear-label` restores the automatic name.

Pooling starts automatically after enrollment. You can keep using Codex normally; `/account` shows the available profiles and selection controls.

## Manage accounts in one place

Start the dedicated account dashboard on the host:

```sh
codex account manage
```

It opens a paired browser page with subscription quota, availability, refresh results, account editing, login, reset credits, pool settings and separate API accounts. **Decision assistance** lets you choose either Jev or Clef in the interface; Cloudflare needs an Account ID and independent token. Press **D** in the terminal manager. On a terminal-only host use `codex account manage --tui`. The manager remains available when the pool is empty or exhausted. For phone access and recovery steps, see the [account manager guide](ACCOUNT_MANAGER.md).

In a terminal, `codex account list` and `codex account status` use aligned tables or compact cards. Add `--details` for cache times and explanations, `--format json` for structured data, or `--format tsv` for tab-separated output. Piped auto output keeps the existing TSV format.

## Keep Remote Control sign-in separate

The **host sign-in** identifies this computer to Remote Control. The **inference pool** supplies quota for tasks. Pool rotation changes inference accounts while keeping the host sign-in source stable. A computer with only enrolled pool accounts still needs a host sign-in source before Remote authentication is available.

Use an enrolled subscription profile as the host sign-in without copying its credentials into the root login:

```sh
codex account primary use "Work"
codex account primary status
```

The browser manager exposes **Host sign-in / Remote Control** and **Use for host sign-in** in account details. In the standalone terminal manager, choose an account and press **H**, then confirm with `APPLY`. A profile can serve as host sign-in even when it is disabled for inference or cooling down. Choosing it does not select it for inference.

Use `codex account primary root` to return to the root credentials created by `codex login`, or `codex account primary logout` to sign out the host while retaining pool credentials. A new successful `codex login` selects the root login again. Removing the profile currently used for host sign-in is blocked until you choose another source or sign out.

Explicit host selection is applied by running hosts without restarting Codex. An already enabled Remote service reconnects for the selected owner; a disabled service stays disabled. Old device authorization is not transferred, so a different owner may require pairing again. `status` distinguishes saved credentials from a recent runtime confirmation.

Older `legacy-root` pool entries stay visible after upgrading. Remove that entry explicitly if you want a separate pool; its removal retains root credentials. New enrollments do not create it. The phone must match the selected host account and workspace; changing that identity can require reconnecting or pairing again. Stored authentication being ready does not prove that a Remote device connection is paired.

## Everyday controls

| Goal                       | CLI on the host                                             | TUI inside Codex                        | Mobile remote client               |
| -------------------------- | ----------------------------------------------------------- | --------------------------------------- | ---------------------------------- |
| See accounts               | `codex account list`                                        | `/account`                              | `/account` or `/account list`      |
| Inspect quota              | `codex account status` (alias: `pool`)                      | `/status` and `/account`                | `/status`; `/account show "Work"`  |
| Select an account          | `codex account use "Work"`                                  | Select Work in `/account`               | `/account use "Work"`              |
| Let the scheduler select   | Use `/account` in Codex                                     | Select Choose automatically on Accounts | `/account auto`                    |
| Choose a rotation strategy | `codex account config set-rotation-strategy earliest-reset` | Choose an entry on Strategy             | `/account strategy earliest-reset` |
| Inspect pool settings      | `codex account config show`                                 | Open Help in `/account`                 | `/account settings`                |
| Repair a login             | `codex account login "Work"`                                | Run the CLI command on the host         | Run the CLI command on the host    |
| Save an account for later  | `codex account disable "Work"`                              | Run the CLI command on the host         | Run the CLI command on the host    |
| Bring it back              | `codex account enable "Work"`                               | Run the CLI command on the host         | Run the CLI command on the host    |

Selection applies to subsequent requests and keeps automatic failover enabled. It does not permanently pin a thread to an account. Pools sharing the same `CODEX_HOME` also share their active selection and cooldown state.

Use quoted labels with spaces. CLI selectors accept an exact profile ID, a unique custom label, or a unique account email. If an email matches several seats or another account's label, use an exact ID or distinct labels; ambiguous matches are rejected. `codex account list --show-profile` reveals IDs for scripts or duplicate names. Mobile accepts the `@selector` shown by `/account list`, which also distinguishes accounts sharing an email. JSON/TSV keep the stored nullable label rather than turning an automatic email into a custom label.

CLI `use` and mobile `/account use` respect a quota cooldown. If you have confirmed that an account's quota recovered, explicitly retry it with `codex account use "Work" --force` or `/account retry "Work"` on mobile. This clears a local cooldown so Codex can probe again; the server's actual quota limit still applies. Cooling accounts in the TUI are explicitly labeled `Retry`, with the retry effect shown when selected.

The native mobile `/status` popup shows the current account's quota. The fork supplies a short pool preview, but the app controls its rendering and may truncate or ignore it; this popup is not a dependable view of every account. Use `/account` or `/status pool` for a local, paginated account list with usage, reset times, observation timestamps and account controls. These percentages remain per-account observations and are not added into a pool balance.

The TUI `/account` picker has **Accounts**, **Strategy**, and **Help** tabs. Follow the footer's key hints to change tabs and select an entry; type to filter account names or emails. Help explains the current reserve, waiting, warmup, and reset-credit settings. Parked profiles and profiles requiring login include a host CLI command for recovery.

## Use quota fully and keep the pool available longer

The default strategy, **fill-first**, follows account priority: smaller numbers are preferred. For example:

```sh
codex account set "Personal" --priority 0
codex account set "Work" --priority 10
```

**earliest-reset** selects eligible accounts whose observed reset is due soonest, preferring idle primary windows so their countdown can start. It can help when you use the pool steadily throughout the day. Reset times and available quota come from observations; they are not a guarantee of runtime. Choose this strategy with the command in the table above.

Codex normally switches at **95% observed usage** when another account is eligible. The remaining quota stays available as a fallback after other accounts run out. Changing the threshold trades a larger margin against more account switches; it does not increase any account's quota.

**Standby warmup** is enabled by default. It sends a small generating request to an eligible idle standby account to try to start its primary window without changing the account used by your task. Maintenance uses the lowest reasoning effort supported by its model and asks for a one-digit reply. This still consumes some quota. Completed or interrupted attempts are protected from repeated generation for five hours while quota-only checks can confirm their result. Near an early switch, warmup checks can accelerate to five-minute intervals. Leave it enabled for sustained use; turn it off when preserving every unused standby allocation matters more than starting its countdown early.

A model-specific limit keeps the account available for other models; follow the backend's switch-model message. If an account does not include the requested use, Codex tries another eligible account and records a short cooldown. Such entitlement errors do not justify spending reset credits. Workspace-wide backend limits can still affect several seats even when their observed percentages differ.

**Reset credits** are saved by default. Automatic redemption is opt-in and only considered after the whole pool is exhausted, when the next backend-confirmed natural reset is farther away than the configured threshold or no natural reset is known. A local retry deadline is only a time to probe again; it does not prove a free reset. If the last failed account has no credit, other exhausted accounts are checked until one recovers; an ambiguous redemption stops the pass. A manual redemption uses a limited credit for that account; refreshing quota or selecting it with `--force` does not create new quota.

When all accounts run out, Codex can **wait and continue safely** after quota recovers. The default maximum wait is six hours. The original host process must remain running; stopping it ends the wait. You can cancel at any time. Visible partial output and unresolved tool results can require reconciliation instead of automatic continuation.

An account used earlier in a long task can re-enter after its cooldown ends. Before reporting whole-pool exhaustion, Codex also makes a bounded metadata recovery pass, including accounts beyond the ordinary small background batch. Confirmed free quota is preferred to reset credits or paid fallback. If the usage endpoint says an account is available but inference keeps refusing it, retries are limited instead of rotating indefinitely. A timeout or partial check is not treated as proof that every subscription is exhausted.

Mobile percentages labeled **Used** and CLI `5H%` / `WEEK%` show usage. TUI labels ending in **left** show the remaining percentage. Primary is usually a rolling 5-hour window and secondary is usually weekly; either window can limit an account. Compare each account's windows separately: adding percentages across plans does not produce a meaningful pool balance. Unknown or cached values remain observations; a failed refresh does not mean an account has 0% usage.

Primary and secondary observations have independent ages. Refreshing one window does not make the other window fresh. `codex account status` reads cached observations and reports their times and resets; it does not contact the quota backend. Mobile `/account` and the `accountPool/read` API can refresh quota metadata. During remote use, meaningful standby changes publish a bounded pool summary; timestamp-only refreshes do not repeat it.

## Understand standby warmup

Keep a Codex session or connected app-server running for automatic warmup; a one-shot `codex account status` command does not keep the scheduler alive. The first automatic pass waits about 30 seconds. Each pass checks at most one standby account, with a default interval of five minutes. Several standby accounts are handled over successive passes. The current account, disabled profiles, profiles needing login, depleted quota, and protected attempts are excluded from generating requests. Processes sharing `CODEX_HOME` coordinate attempts so they do not all warm the same account.

Request completion and an observed 5-hour window start are different. A tiny completed request can still have 0% observed usage, and an idle account can receive a reset timestamp even before its clock starts. `start unconfirmed` reports this uncertainty. Positive primary usage confirms a window was observed; a weekly countdown does not confirm the primary window. Day countdowns use explicit units, for example `4d 3h 43m`.

| Warmup status                       | Meaning                                                                                                                                                                  |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `warmup in progress`                | A recent attempt is running; the selection used by your task stays unchanged.                                                                                            |
| `warmup sent; start unconfirmed`    | A generating request was sent and may have completed or been interrupted; primary usage has not confirmed the clock. Quota checks can continue without generating again. |
| `warmup attempt; start unconfirmed` | An earlier attempt may have been interrupted. Its generating retry remains protected while the result is checked.                                                        |
| `warmup failed; retry in …`         | The attempt failed and is backing off; the shown time is an earliest retry, not a quota reset.                                                                           |
| `warmup deferred; check in …`       | Preflight found the account temporarily ineligible; no generating request was sent.                                                                                      |
| `warmup retry ready`                | The failure backoff ended. A later eligible pass can retry.                                                                                                              |
| `warmup needs login`                | Repair that profile with `codex account login "Work"`.                                                                                                                   |
| `5h confirmed`                      | Recent warmup and positive primary usage agree.                                                                                                                          |

In the TUI, run `/warmup` to inspect effective settings, whether the task is running, each account's eligibility or skip reason, persisted attempts, and recent events. Events belong to the current host process; persisted attempt protection survives restarts. On mobile, `/account warmup` reads the summary. Both are read-only diagnostics and do not send a generating request.

`/warmup now` in the TUI or `/account warmup now` on mobile asks for one pass. It respects warmup being off, account eligibility, and existing backoff; it can consume quota if a generating request is needed. It returns before the pass finishes, so read the summary again afterward. Repeated commands do not bypass the protection period. To disable warmup, use `codex account config set-window-warmup false` on the host or `/account warmup off` on mobile.

## Configure the behavior

Run `codex account config show` to inspect effective settings. CLI setters save settings in `[account_pool]` in `${CODEX_HOME}/config.toml` (normally `~/.codex/config.toml`); you can also edit that section directly. These are the defaults:

```toml
[account_pool]
rotation_strategy = "fill_first" # or "earliest_reset"
return_to_preferred = true
preemptive_switch_percent = 95
window_warmup = true
window_warmup_interval_minutes = 5
resume_after_reset = true
max_reset_wait_minutes = 360
auto_reset_credits = "never" # or "when_pool_exhausted"
auto_reset_credit_min_wait_minutes = 60
```

| Setting                              | Meaning                                                                                                                      |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- |
| `return_to_preferred`                | Return to a lower-priority-number account after its cooldown expires. Set `false` to stay with the current eligible account. |
| `preemptive_switch_percent`          | Early rotation threshold. Set `0` to disable early rotation; hard-limit failover stays enabled.                              |
| `window_warmup_interval_minutes`     | Cadence for background warmup passes; values below 5 are clamped to 5.                                                       |
| `resume_after_reset`                 | Enable cancellable waiting for an exhausted pooled turn.                                                                     |
| `max_reset_wait_minutes`             | Cumulative waiting allowance for one turn across retries and sampling steps, capped at 1440 minutes; `0` disables waiting.   |
| `auto_reset_credits`                 | `never` saves credits; `when_pool_exhausted` enables the rule described above.                                               |
| `auto_reset_credit_min_wait_minutes` | Skip automatic redemption when a natural reset is within this many minutes.                                                  |

Each setting also has a CLI setter:

```sh
codex account config set-rotation-strategy earliest-reset
codex account config set-return-to-preferred false
codex account config set-preemptive-switch-percent 95
codex account config set-window-warmup false
codex account config set-window-warmup-interval-minutes 10
codex account config set-resume-after-reset true
codex account config set-max-reset-wait-minutes 360
codex account config set-auto-reset-credits never
codex account config set-auto-reset-credit-min-wait-minutes 60
```

These are independent examples. Check `config show` after a change: higher-priority configuration or launch overrides can change the effective value. Warmup cadence setters accept 5–1440 minutes; wait setters accept 0–1440.

To favor uninterrupted use, try `earliest_reset` with warmup enabled. To preserve standby quota, keep `fill_first` and set `window_warmup = false`. To avoid returning to a preferred account mid-session, set `return_to_preferred = false`. Start with one change and inspect its effect in `/account`.

## Use from a phone

Configure profiles on the remote host first, then start remote control from the installed fork:

```sh
codex remote-control start
```

Follow the pairing information that it prints. If needed, `codex remote-control pair` creates a short-lived pairing code. Remote Control follows the host sign-in source shown by `codex account primary status`; execution-pool rotation does not re-pair the phone.

In a supported ChatGPT iOS or Android remote client:

```text
/account help
/status pool
/status pool 2
/account list 2
/account show "Work"
/account use "Work"
/account auto
/account strategy
/account settings
/account warmup off
/account resume on
/account wait 360
/account reset-credits never
```

The list shows and refreshes four accounts per page; copy its `@selector` for an unambiguous detail or selection command. Cooling accounts offer `retry` to probe quota; disabled or expired logins require action on the host. `cached` means the displayed observation was not refreshed recently, and a reset being due does not prove recovery. Detail queries refresh only the selected account. `/account` and `/status pool` are handled locally by the host and do not send an inference request or enter the model's conversation history. The app may intercept plain `/status` to open its native panel. Queries can still make quota metadata requests. During an active turn, use the app's Status panel or wait for the turn to finish before issuing these chat commands.

The fork supplies pool captions, status replies, and controls through interfaces the mobile client already renders. It cannot replace the app's native screens. The `/status pool` alias requires a host build newer than ma.5; on ma.5, use `/account`. If the app also intercepts `/status pool`, use `/account` instead. Real device behavior can differ across app versions; iOS reconnect acceptance remains pending.

## Understand status and recover

| Status or symptom                           | What to do                                                                                                                                            |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Available / ready                           | Eligible for scheduling; observed quota can still be unknown or outdated.                                                                             |
| Cooling down / exhausted                    | Wait for reset or let the scheduler choose another account.                                                                                           |
| Login required / auth unavailable           | Run `codex account login "Work"`; use the same user and workspace.                                                                                    |
| Disabled                                    | Intentionally parked. Run `codex account enable "Work"` to schedule it again.                                                                         |
| Cached / unknown quota                      | Inspect the account again. Retained cached values are not a fresh quota check.                                                                        |
| Pool remains paused after logout            | Explicitly select a profile with `codex account use "Work"` to resume pooling.                                                                        |
| `/account` is unavailable after an update   | Verify the `+ma.N` binary on PATH, stop the managed daemon with `codex app-server daemon stop`, and relaunch. For mobile, start remote control again. |
| Old thread reports opaque compacted history | Switch to its owning account and run `/compact` before using another account.                                                                         |

Re-login stages the new credentials before replacement and verifies the same user and workspace. For a different account or seat, use `account add`. Removing and re-adding a profile is usually unnecessary to repair login.

Use `codex account primary logout` for a host-only sign-out that keeps the pool. When root login is the active source, the existing `codex logout` flow still pauses pooling across restarts while retaining enrolled profiles. `codex account remove "Work"` removes one profile and normally deletes/revokes its stored credentials; `--keep-credentials` retains them. `disable` is the reversible way to reserve an account without removing its login.

## Update

Run the fork installer again, or download a newer matching release bundle. To pin a version, append its exact tag:

```sh
curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash -s -- rust-vX.Y.Z-ma.N
```

The macOS/Linux installer validates published SHA256 checksums, installs the helpers with the main binary, and stops a stale managed daemon. Restart remote control afterward if you use it. `codex update` directs you to the fork's releases.

Account profiles and credentials stay on the host under `CODEX_HOME`; credential storage follows the configured file/keyring mode. Keep that directory private. API-key provider configuration does not turn an API key into a subscription-pool profile.

The [Jev/Clef decision-model research](DECISION_MODELS.md) explains potential tool-selection and reasoning-effort improvements, their cost, and what needs measurement. They default to off; choose one in the manager, with no simultaneous calls. They do not replace account identity, quota, or scheduling rules.

Cross-workspace concurrent routing, automatic migration of old opaque compacted history, and real Personal/Business/mobile acceptance still need further work. Automated fixture coverage and release archive checks do not establish those real-account behaviors.
