# Account manager

[简体中文](ACCOUNT_MANAGER.zh-CN.md) · [User guide](README.md)

## Open the manager

Run on the host that stores your Codex accounts:

```sh
codex account manage
# Terminal-only alternative:
codex account manage --tui
# Print the browser URL without opening it:
codex account manage --no-open
```

The browser manager is a separate process. Keep it running; Ctrl+C closes it. It shares profiles, selection and quota observations with Codex processes using the same `CODEX_HOME`. Existing requests keep their captured identity; changes apply at safe subsequent request boundaries.

The default address is loopback with a random port. The printed link pairs your browser; its code grants account administration. The code is removed from the URL after pairing and stored in origin-isolated browser session storage, not cookies or local storage. Pair again after restarting the manager.

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

## Read the dashboard

Subscription accounts have separate **login** and **availability** states. Being enrolled or signed in does not prove that quota is usable. Disabled accounts and pending logins remain visible. A paused pool is displayed separately.

Quota bars show **used** quota for each account. Unknown quota stays unknown. Each window displays its own cache age and reset time. Reading the page every few seconds reads host metadata; it does not generate model output, use a reset credit or continually refresh every account's credentials.

**Refresh quota** contacts each enabled account's usage endpoint. Inspect each account's result if a request failed. A failed refresh retains the old values. Current quota percentages are advisory; request refusals remain authoritative until a fresh, complete, matching backend response confirms recovery.

Labels and exact profile IDs are separate. Search labels, email or IDs; open details for less frequently used information. Desktop tables become cards on narrow screens.

## Restore an exhausted pool

If you already redeemed a reset credit elsewhere:

1. Open the manager and choose **Refresh quota**.
2. A complete backend response that explicitly permits usage for the same seat clears its stale local exhaustion and replaces the old quota windows.
3. If the backend response is incomplete, use **Retry after external reset** for the intended account. This clears its local cooldown for a new request; the backend can still reject usage. It does not consume a credit.

An exhausted new turn also makes a bounded passive usage check. A turn already waiting for quota checks periodically, so it can discover an external reset without waiting for the old cached deadline. Cancellation still stops waiting. This is in-process continuation; stopping the host process does not persist a running task.

If a new refusal or relogin races a refresh, the newer account state wins. The next fresh check can retry. A workspace entitlement or login failure is not treated as ordinary quota recovery.

To redeem through the manager, open the exact account's **Reset credits** view, select an available credit, review the target and confirm. The operation does not globally switch your active account first. Expired or unavailable credits cannot be chosen. If the result is unconfirmed after a connection interruption, refresh quota and review the same operation before retrying; the browser preserves its operation ID, not your credentials.

**Refresh**, **Retry**, and **Redeem** have different effects. Redeem spends a credit; the other two do not. Returning to automatic subscriptions keeps valid cooldowns.

## Manage logins and accounts

Use Add account for device login. Open the verification URL and enter the code; the dashboard tracks completion. Relogin keeps the existing seat and refuses a different workspace identity. Two Business users in one workspace stay distinct. Adding the same user and workspace refreshes its existing profile instead of creating another usable quota entry.

Edit labels and priority, enable or disable scheduling, and remove profiles from the same page. Removing local credentials attempts server revocation; local removal does not prove that the remote revocation succeeded. Pending-login cancellation cleans the selected credential storage without revoking credentials that were copied into an existing profile.

The standalone terminal manager offers the same operations through numbered accounts and explicit action menus. Invalid input returns to the menu with an explanation. The browser interface provides the most detailed credit and quota layout.

## API accounts and paid fallback

API accounts are separate from ChatGPT subscription scheduling. They do not receive subscription warmup or reset-credit operations. Add the provider's base URL, exact model, key, context window and image capability in **API accounts**. Keys use your configured credential storage; metadata and inventory contain no keys. Replace a key without deleting its profile. Saving does not make a generating compatibility request.

This first integration requires a **native Responses API** endpoint. Enter its base URL such as `https://provider.example/v1`; Codex appends `/responses`. Chat Completions-only endpoints, including direct Gemini OpenAI compatibility endpoints, are not supported by this integration. DeepSeek deployments with native Responses support can be configured with their exact endpoint and model. Provider tools and model capabilities still vary; do not enable images unless supported.

**Use API account** explicitly selects the billed target for subsequent turns. **Return to subscriptions** restores subscription-first automatic selection even while those accounts are exhausted. An API selection remains captured throughout a turn and its tool continuations; global edits do not silently change its provider mid-request.

Paid fallback is **off by default**. To enable it, choose an API account and a subscription waiting limit, then acknowledge provider charges and conversation transfer. On applicable quota exhaustion Codex tries subscription recovery and configured reset-credit rescue, shares a bounded wait budget across the turn, and then uses the explicit fallback. Authentication, entitlement, malformed requests and unsafe replay failures do not authorize paid fallback. A cancelled wait sends no fallback request.

Third-party requests omit subscription headers and foreign encrypted reasoning. Completed tool results remain in context and are not replayed just because a provider changed. Opaque compacted history may require a fresh thread before changing providers. This boundary preserves local conversation history while adapting the outgoing request.

Managed authentication, provider requirements, network, sandbox and approval policies continue to apply. Disabling or removing a selected API profile makes subsequent selection unavailable; choose subscriptions or another enabled API profile.
