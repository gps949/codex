# Account manager

[简体中文](ACCOUNT_MANAGER.zh-CN.md) · [User guide](README.md)

## Open the manager

Run on the host that stores your Codex accounts:

```sh
codex account manage
# Start the browser manager in Simplified Chinese:
codex account manage --lang zh-CN
# Terminal-only alternative:
codex account manage --tui
# Chinese terminal interface:
codex account manage --tui --lang zh-CN
# Print the browser URL without opening it:
codex account manage --no-open
```

The browser manager is a separate process. Keep it running; Ctrl+C closes it. It shares profiles, selection and quota observations with Codex processes using the same `CODEX_HOME`. Existing requests keep their captured identity; changes apply at safe subsequent request boundaries.

The default address is loopback with a random port. The printed link pairs your browser; its code grants account administration. The code is removed from the URL after pairing and stored in origin-isolated browser session storage, not cookies or local storage. Pair again after restarting the manager.

### Inside the official mobile Remote app

Use `/account` or `/account manage` in a Remote conversation. `/account manage zh-CN` chooses Chinese for that session. The native menu uses short headings and regular option descriptions, with four accounts per page. Open an account to inspect quota, refresh it, select it, retry after an external quota reset, enable or disable it, rename it, relogin, or remove it. Changes require a confirmation showing the target. Reset credits have their own list and per-credit confirmation; checking a credit does not consume it. API use and paid fallback stay explicit. The menu also offers pool settings, adding a subscription and choosing the host sign-in source.

If the client sends `/account manage` as a steering message during a running turn, the host opens a nonblocking menu attached to that turn without submitting the command to the model or replacing the task. The fork cannot add a native toolbar button or register a new client-side panel in the official mobile app. Actual availability during output depends on whether that app version sends steering requests and displays nonblocking questions. `/account list` remains the compact text fallback.

Use `/account capabilities` to inspect this connection's declared UI formats and observed native-question response. A successful response establishes a protocol round trip; official iOS/Android rendering and tap behavior still require device verification. A timeout only means no answer arrived. An explicit method-not-found response means this connection did not handle native questions; `/account list` and `/account show <label>` remain available.

Questions expire after 90 seconds without an answer; a menu lasts at most ten minutes. Stop, a new message, disconnect or unsubscribe cancels its outstanding questions. Closing an attached menu leaves the real task running. Reconnect with `/account manage`; menu pages are not saved into model context or restored conversation history. Browsing reuses one short message anchor instead of posting every page to the conversation.

The existing WebUI and full terminal manager still run separately. Remote does not forward their HTTP port or terminal keyboard input. The native menu uses the conversation's question controls.

### External phone browser or SSH access

For access from a phone, prefer an SSH tunnel or your own authenticated HTTPS reverse proxy. Choose a fixed host port if needed:

```sh
codex account manage --listen 127.0.0.1:8765 --no-open
```

An SSH tunnel can forward your local port to host port 8765. Open the printed link with the forwarded port. A non-loopback listener requires an explicit HTTPS browser origin:

```sh
codex account manage --listen 0.0.0.0:8765 \
  --allow-origin https://codex-accounts.example.com --no-open
```

Configure that reverse proxy yourself and preserve its browser Host header. Use the HTTPS origin in your browser, then paste the pairing code from the host link. This listener exposes account management only. It does not replace the native remote app-server connection. The official mobile `/status` popup is controlled by the mobile app; the dashboard gives you a separate full account view.

## Start with the next step

The overview recommends a next action for the current pool: add your first account, finish a login, refresh exhausted subscriptions, resume a paused pool, or return from a billed API account. Each account also has a primary action suited to its current status. Advanced information stays in account details.

The browser defaults to English. Use `--lang zh-CN` for the opening language or the language selector for Simplified Chinese; your account labels, URLs, model names and IDs remain unchanged. The choice applies to this browser.

## Read the dashboard

Subscription accounts have separate **login** and **availability** states. Being enrolled or signed in does not prove that quota is usable. Disabled accounts and pending logins remain visible. A paused pool is displayed separately.

Quota bars show **used** quota for each account. Unknown quota stays unknown. Each window displays its own cache age and reset time using the host clock. Zero usage with a reset timestamp does not prove that a window or warmup request has started. Future cache timestamps are flagged so they cannot look freshly checked. Adaptive polling stops while the browser tab is hidden. Reading the page reads host metadata; it does not generate model output, use a reset credit or continually refresh every account's credentials.

**Refresh quota** contacts each enabled account's usage endpoint. Progress shows accounts still checking and the completion summary counts updates and failures. Repeated refreshes join existing per-account checks. Inspect each account's result if a request failed. A failed refresh retains the old values. Current quota percentages are advisory; request refusals remain authoritative until a fresh, complete, matching backend response confirms recovery.

Labels and exact profile IDs are separate. Without a custom label, the full account email is the display name; a missing email falls back to the profile ID. Search labels, email or IDs; open details for less frequently used information. Desktop tables become cards on narrow screens. Accounts sharing an email still have distinct profile IDs.

## Restore an exhausted pool

If you already redeemed a reset credit elsewhere:

1. Open the manager and choose **Refresh quota**.
2. A complete backend response that explicitly permits usage for the same seat clears its stale local exhaustion and replaces the old quota windows.
3. If the backend response is incomplete, use **Retry without credit** for the intended account. This clears its local cooldown for a new request; the backend can still reject usage. It does not consume a credit.

An exhausted new turn also makes a bounded passive usage check. A turn already waiting for quota checks periodically, so it can discover an external reset without waiting for the old cached deadline. Cancellation still stops waiting. This is in-process continuation; stopping the host process does not persist a running task.

Earlier accounts can re-enter during the same long task when their cooldown ends; having failed once does not permanently exclude them from that task. A final bounded recovery check also reaches beyond the ordinary small background batch before declaring whole-pool exhaustion. Recovery stops on usable free quota. Repeated inference refusals despite positive usage metadata are limited to prevent a retry loop; incomplete coverage is reported as unconfirmed, rather than proof that all accounts remain exhausted.

If a new refusal or relogin races a refresh, the newer account state wins. The next fresh check can retry. A workspace entitlement or login failure is not treated as ordinary quota recovery.

To redeem through the manager, open the exact account's **Use reset credit** view, select an available credit, review the target and confirm. The operation does not globally switch your active account first. Expired or unavailable credits cannot be chosen. If the result is unconfirmed after a connection interruption, refresh quota and review the same operation before retrying; the browser preserves its operation ID, not your credentials.

**Refresh**, **Retry**, and **Redeem** have different effects. Redeem spends a credit; the other two do not. Returning to automatic subscriptions keeps valid cooldowns.

## Manage logins and accounts

Use Add account for device login. Open the verification URL and enter the code; the dashboard tracks completion. Relogin keeps the existing seat and refuses a different workspace identity. Two Business users in one workspace stay distinct. Adding the same user and workspace refreshes its existing profile instead of creating another usable quota entry.

Edit labels and priority, enable or disable scheduling, and remove profiles from the same page. Removing local credentials attempts server revocation; local removal does not prove that the remote revocation succeeded. Pending-login cancellation cleans the selected credential storage without revoking credentials that were copied into an existing profile.

**Edit label & priority** shows only the saved custom label in its name field. Leave it empty for an automatic email name. Changing priority alone keeps that choice; clearing an existing name restores the email. In the standalone terminal manager, **E** edits while Enter retains the current custom label, and **N** explicitly clears it. A CLI equivalent is `codex account set <profile-id> --clear-label`.

The standalone terminal manager offers numbered accounts, login tasks and reset credits with explicit action menus. It defaults to English; use `--lang zh-CN` at startup or press **G** to switch between English and Simplified Chinese. Commands, confirmation words, account names and identifiers keep their original values. Verification codes appear while you wait at the main menu. Credit errors retain the same operation ID for review and retry; record the printed ID before closing the manager. Invalid input returns to the menu with an explanation. The browser interface provides the most detailed credit and quota layout.

## Choose the Remote Control host sign-in

The **Host sign-in / Remote Control** panel shows the login source separately from the inference selection. In account details, choose **Use for host sign-in**, verify the displayed account, email and exact profile ID, then confirm. In the terminal manager, choose the account and press **H**, then type `APPLY`. A completed subscription login is required; an inference cooldown or disabled inference scheduling does not prevent using its identity for host sign-in.

This source stays fixed while pool inference rotates. It refers to the selected profile's credentials instead of copying tokens into the root home. Adding the first pool account does not import the root login, and enrollment does not select a host identity automatically. With no explicit selection, an existing root login remains the default host source.

**Use root login** returns to credentials saved by `codex login`. **Sign out host** retains the pool's saved credentials and inference selection. The corresponding CLI commands are:

```sh
codex account primary status
codex account primary use <label-or-email-or-profile-id>
codex account primary root
codex account primary logout
```

Removing a profile selected for host sign-in is blocked. Select another source or sign out first. Older `legacy-root` entries are retained during upgrade; removing that pool entry keeps root credentials. New enrollment does not create an imported entry.

Stored host credentials being available is a local status, not confirmation of a device connection. The official phone app must use the matching account and workspace; an identity change can require reconnecting or pairing again. If the selected login becomes unavailable or changes owner, host authentication stops instead of silently choosing a different pool identity. Repair the selected login or explicitly choose another source.

## Choose settings without learning the config format

The browser offers **Prefer earlier resets** and **Reduce standby requests** with a preview of changed fields. The terminal uses named settings and **Balanced** / **Longer standby** presets; these enable small standby warmup requests and show the exact configuration before applying it. All presets preserve your reset-credit and paid-API permissions. Saving requires confirmation. Advanced settings remain available for individual tuning.

## API accounts and paid fallback

API accounts are separate from ChatGPT subscription scheduling. They do not receive subscription warmup or reset-credit operations. Add the provider's base URL, exact model, key, context window and image capability in **API accounts**. Keys use your configured credential storage; metadata and inventory contain no keys. Replace a key without deleting its profile. Saving does not make a generating compatibility request. Endpoint, model and key are the basic fields; context size and image input are advanced capabilities. Automatic and manual compaction follow the captured API model and credentials.

This first integration requires a **native Responses API** endpoint. Enter its base URL such as `https://provider.example/v1`; Codex appends `/responses`. Chat Completions-only endpoints, including direct Gemini OpenAI compatibility endpoints, are not supported by this integration. DeepSeek deployments with native Responses support can be configured with their exact endpoint and model. Provider tools and model capabilities still vary; do not enable images unless supported.

**Use API account** explicitly selects the billed target for subsequent turns. **Return to subscriptions** restores subscription-first automatic selection even while those accounts are exhausted. An API selection remains captured throughout a turn and its tool continuations; global edits do not silently change its provider mid-request.

Paid fallback is **off by default**. To enable it, choose an API account and a subscription waiting limit, then acknowledge provider charges and conversation transfer. On applicable quota exhaustion Codex tries subscription recovery and configured reset-credit rescue, shares a bounded wait budget across the turn, and then uses the explicit fallback. Authentication, entitlement, malformed requests and unsafe replay failures do not authorize paid fallback. A cancelled wait sends no fallback request.

Third-party requests omit subscription headers and foreign encrypted reasoning. Completed tool results remain in context and are not replayed just because a provider changed. Opaque compacted history may require a fresh thread before changing providers. This boundary preserves local conversation history while adapting the outgoing request.

Managed authentication, provider requirements, network, sandbox and approval policies continue to apply. Disabling or removing a selected API profile makes subsequent selection unavailable; choose subscriptions or another enabled API profile.

Before an automatic reset credit or paid fallback is started, Codex makes a bounded metadata check of every eligible exhausted seat. A recovered free subscription wins. An incomplete or failed coverage check prevents automatic spending. Large pools continue passive rotation; they can be refreshed or selected manually.

Disabling paid fallback or changing its selected account while a turn waits cancels the pending paid transition. The first outbound request checks authorization again. Requests and tool continuations that have already started retain their captured destination.

## Closing the browser manager

Choose **Stop manager** to stop its host listener and all paired tabs, or press Ctrl+C in its terminal. Unix hosts also handle SIGTERM. Pending logins and read requests are cancelled; a change already being written finishes safely before exit. Other Codex sessions keep running.

Closing the last tab normally starts a 30-second grace period, allowing refresh or reopening. Other live tabs keep the manager running. A crashed or frozen page loses its lease after five minutes, followed by the same grace period. A manager never opened stops after ten minutes. The browser itself is managed by your operating system.

## Reading the command-line inventory

`codex account list` shows **used** quota. `UPDATED` is the age of the oldest displayed quota sample; each window's exact observation and reset time appears with `--details`. An elapsed reset marks the old percentage `stale` until a fresh authenticated query confirms current usage. `retry 3h29m` is the local scheduling delay, not a confirmed backend reset. Window headings use reported durations; unknown or mixed durations use primary/secondary labels. Full profile IDs remain available with `--show-profile`, and JSON/TSV output is unchanged.

## Expiring reset credits

During ordinary user turns, the host checks a small rotating batch of subscription profiles using authenticated credit metadata. A passive notice appears once per credit at the current 24/18/12/6/3/1-hour stage, at most six times for that credit. Missed stages are skipped; restart and duplicate profiles do not restart the notice budget. No credit is consumed by a notice. Fast completed turns can populate the cache for the next active turn without showing a late warning. Review and redeem explicitly in the dedicated manager; the native in-app menu supports credit listing and explicitly confirmed redemption.

Language choices made inside the WebUI or TUI are remembered in the host's nonsecret manager preferences, across random ports and restarts. English is the default; startup language options are session overrides. In the TUI, **H** opens host sign-in management and **W** shows per-account warmup evidence. Stored login availability and the most recent running host/Remote status are shown separately; an expired or ambiguous heartbeat is reported as unknown.

Common browser actions are available directly from account rows or the open details page. **Use reset credit** preselects the eligible credit with the earliest reported expiry and opens its confirmation: normally two clicks from the account list to confirmed use. The confirmation names the exact account, scope and expiry; **Choose another credit** remains available. An earlier unconfirmed operation keeps its original credit and operation ID instead of silently selecting another credit. Credits with no reported expiry follow dated credits. Removing pool membership while keeping login and removing it with managed-login deletion are distinct actions; root login is retained for legacy-root entries.
