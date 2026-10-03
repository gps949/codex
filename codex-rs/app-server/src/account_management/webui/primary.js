"use strict";

(() => {
  const messages = window.AccountManagerMessages;
  const { t } = messages;
  function node(tag, text = "", className = "") {
    const element = document.createElement(tag);
    element.textContent = text;
    element.className = className;
    return element;
  }
  function render(view, confirm, { busy = false } = {}) {
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
    body.append(node("p", identity, "account-name"));
    if (view.email && view.email !== identity)
      body.append(node("p", view.email, "muted"));
    body.append(
      node(
        "p",
        t(
          view.ready
            ? "Stored host sign-in is available"
            : "Host sign-in needs attention",
        ),
      ),
    );
    if (!view.ready && view.source !== "signedOut")
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
    body.append(
      node(
        "small",
        t(
          "Inference accounts rotate independently. A host login change may require Remote Control reconnection or pairing.",
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
          "This changes host sign-in only. Pool inference and its saved credentials stay separate. Existing Remote Control connections may need to reconnect.",
          { type },
        ),
      );
      actions.append(button);
    }
    body.append(actions);
  }
  window.AccountManagerPrimary = Object.freeze({ render });
})();
