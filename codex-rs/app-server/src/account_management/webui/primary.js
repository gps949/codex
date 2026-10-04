"use strict";

(() => {
  const messages = window.AccountManagerMessages;
  const { t } = messages;
  function node(tag, text = "", className = "") {
    const element = document.createElement(tag);
    element.textContent = String(text).replace(
      /[\u0000-\u001f\u007f-\u009f\u061c\u200b\u200e\u200f\u2028-\u202e\u2060\u2066-\u2069\ufeff]/g,
      " ",
    );
    element.className = className;
    return element;
  }
  function updateRuntime(view, { hostNow = 0 } = {}) {
    const body = document.getElementById("host-runtime");
    if (!body || !view) return;
    body.replaceChildren();
    const runtime = view.runtime;
    if (
      runtime &&
      runtime.sourceRevision === view.revision &&
      Number.isFinite(runtime.observedAt) &&
      runtime.observedAt <= hostNow &&
      hostNow - runtime.observedAt < 10
    ) {
      body.append(
        node(
          "p",
          t("Observed host identity: {identity}", {
            identity: runtime.email || t("Not reported"),
          }),
        ),
      );
      const statuses = {
        disabled: "Disabled",
        connecting: "Connecting",
        connected: "Connected to relay",
        errored: "Connection needs attention",
        requirementsDisabled: "Remote disabled by account requirements",
        authenticationDenied: "Host authentication denied by requirements",
      };
      body.append(
        node(
          "p",
          t("Remote service: {status}", {
            status: t(
              Object.hasOwn(statuses, runtime.remoteStatus)
                ? statuses[runtime.remoteStatus]
                : "Not reported",
            ),
          }),
        ),
      );
    } else {
      body.append(
        node(
          "p",
          t(
            "No recent host confirmation for this selection. Stored credentials do not confirm Remote Control is connected.",
          ),
          "muted",
        ),
      );
    }
  }
  function render(view, confirm, { busy = false, hostNow = 0 } = {}) {
    const body = document.getElementById("host-login");
    if (!body) return;
    body.replaceChildren();
    if (!view) {
      body.hidden = true;
      return;
    }
    body.hidden = false;
    const heading = node("h2", t("Host sign-in / Remote Control"));
    heading.id = "host-login-title";
    body.setAttribute("aria-labelledby", heading.id);
    body.append(heading);
    const identity = view.source === "profile" ? view.label : t(view.label);
    body.append(
      node(
        "p",
        t("Saved source: {label}", { label: identity }),
        "account-name",
      ),
    );
    if (view.email && view.email !== identity)
      body.append(node("p", view.email, "muted"));
    body.append(
      node(
        "p",
        t(
          view.status === "runtimeResolutionRequired"
            ? "Host sign-in is managed by the running host"
            : view.ready
              ? "Stored host sign-in is available"
              : "Host sign-in needs attention",
        ),
      ),
    );
    if (
      !view.ready &&
      view.source !== "signedOut" &&
      view.status !== "runtimeResolutionRequired"
    )
      body.append(
        node(
          "p",
          t(
            "Choose a signed-in subscription account below, or run codex login on this host.",
          ),
          "muted",
        ),
      );
    if (view.profileId)
      body.append(node("small", `${t("Profile ID")}: ${view.profileId}`));
    if (view.message)
      body.append(node("p", t(view.message), "message warning"));
    const runtimeBody = node("div");
    runtimeBody.id = "host-runtime";
    body.append(runtimeBody);
    updateRuntime(view, { hostNow });
    body.append(
      node(
        "small",
        t(
          "Inference accounts rotate independently. An enabled Remote service reconnects after an explicit host login change. A new owner may require phone pairing again.",
        ),
      ),
    );
    const actions = node("div", "", "actions");
    for (const [title, type] of [
      ["Use root login", "primaryRoot"],
      ["Sign out host", "primaryLogout"],
    ]) {
      const button = node("button", t(title));
      button.type = "button";
      button.dataset.focusKey = `host:${type}`;
      button.disabled =
        busy || (type === "primaryLogout" && view.source === "signedOut");
      button.addEventListener("click", () =>
        confirm(
          title,
          type === "primaryLogout"
            ? "Sign out host and disconnect Remote Control. Pool accounts and credentials are retained."
            : "Use root login for host sign-in. An already enabled Remote service reconnects for this owner; a disabled service stays disabled. Pool inference selection is unchanged.",
          { type },
        ),
      );
      actions.append(button);
    }
    body.append(actions);
  }
  window.AccountManagerPrimary = Object.freeze({ render, updateRuntime });
})();
