"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const messages = window.AccountManagerMessages;
  const { t } = messages;
  const guidance = window.AccountManagerGuidance;
  const state = {
    inventory: null,
    busy: false,
    paired: false,
    reading: null,
    timer: null,
    lastRead: 0,
    inventoryKey: "",
    readFailed: false,
    page: 0,
    sessionToken: "",
    stopped: false,
    preferencesLoaded: false,
    preferenceSaving: false,
    languageDirty: false,
  };
  const resetStorageKey = "codex.accountManager.resetOperation";
  const resetOperations = window.AccountManagerResetOperations.create(
    {
      getItem: (key) => sessionStorage.getItem(key),
      setItem: (key, value) => sessionStorage.setItem(key, value),
      removeItem: (key) => sessionStorage.removeItem(key),
    },
    resetStorageKey,
  );
  const sessionStorageKey = "codex.accountManager.sessionToken";
  const pageSize = 8;
  const statusNames = guidance.names;
  let dialogTask = null;
  let dialogAccountId = null;
  let dialogCompleted = false;
  let returnFocus = null;
  let returnFocusKey = null;

  function element(tag, text = "", className = "") {
    const node = document.createElement(tag);
    if (text !== "")
      node.textContent = String(text).replace(
        /[\u0000-\u001f\u007f-\u009f\u061c\u200b\u200e\u200f\u2028-\u202e\u2060\u2066-\u2069\ufeff]/g,
        " ",
      );
    if (className) node.className = className;
    return node;
  }
  function button(text, action, className = "") {
    const node = element("button", t(text), className);
    const glyph = {
      "Use reset credit": "↺",
      "Refresh quota": "↻",
      "Details & actions": "⋯",
    }[text];
    if (glyph) {
      const icon = element("span", glyph, "action-icon");
      icon.setAttribute("aria-hidden", "true");
      node.prepend(icon);
    }
    node.type = "button";
    node.addEventListener("click", action);
    return node;
  }
  function definitions(items) {
    const node = element("dl", "", "detail-list");
    for (const [key, value] of items)
      node.append(element("dt", t(key)), element("dd", value ?? t("Unknown")));
    return node;
  }
  function disclosure(title, ...children) {
    const node = element("details", "", "control-disclosure");
    node.append(element("summary", t(title)), ...children);
    return node;
  }
  function field(body, name, label, type, value, help = "", attributes = {}) {
    const wrapper = element("div", "", "form-field");
    const title = element("label", t(label));
    const input = element(type === "select" ? "select" : "input");
    input.id = `field-${name}`;
    input.name = name;
    title.htmlFor = input.id;
    if (type !== "select") input.type = type;
    if (type === "select") {
      for (const [optionValue, optionLabel] of attributes.options) {
        const option = element("option", optionLabel);
        option.value = optionValue;
        input.append(option);
      }
    }
    if (type === "checkbox") input.checked = value;
    else input.value = value ?? "";
    for (const [key, setting] of Object.entries(attributes))
      if (key !== "options") input[key] = setting;
    if (type === "checkbox") {
      title.className = "check-label";
      title.prepend(input);
      wrapper.append(title);
    } else wrapper.append(title, input);
    if (help) {
      const hint = element("small", t(help));
      hint.id = `${input.id}-help`;
      input.setAttribute("aria-describedby", hint.id);
      wrapper.append(hint);
    }
    body.append(wrapper);
    return input;
  }
  function date(value) {
    if (value === null || value === undefined || value === "")
      return t("Not reported");
    const parsed = new Date(typeof value === "number" ? value * 1000 : value);
    return Number.isNaN(parsed.getTime())
      ? t("Unknown date")
      : parsed.toLocaleString(messages.locale(), {
          year: "numeric",
          month: "short",
          day: "numeric",
          hour: "numeric",
          minute: "2-digit",
          timeZoneName: "short",
        });
  }
  function timedText(kind, value, name = "") {
    const node = element("small");
    node.dataset.timeKind = kind;
    node.dataset.timeValue = JSON.stringify(value ?? null);
    node.dataset.windowName = name;
    updateTime(node);
    return node;
  }
  function updateTime(node) {
    const value = JSON.parse(node.dataset.timeValue);
    node.textContent =
      node.dataset.timeKind === "age"
        ? guidance.age(value)
        : guidance.reset(value, node.dataset.windowName);
  }
  function updateClocks() {
    $("metadata-age").textContent = t("Host status read at {time}", {
      time: new Date(state.lastRead).toLocaleTimeString(messages.locale()),
    });
    for (const node of document.querySelectorAll("[data-time-kind]"))
      updateTime(node);
    window.AccountManagerPrimary.updateRuntime(state.inventory?.primaryLogin, {
      hostNow: Math.floor(guidance.now()),
    });
  }
  function quota(account, name) {
    const limits = account.rateLimits || {};
    const window = limits[name];
    const minutes = window?.windowMinutes;
    const label =
      minutes > 0
        ? minutes % 10080 === 0
          ? t("{count} week", { count: minutes / 10080 })
          : minutes % 1440 === 0
            ? t("{count} day", { count: minutes / 1440 })
            : minutes % 60 === 0
              ? t("{count} hr", { count: minutes / 60 })
              : t("{count} min", { count: minutes })
        : t(name === "primary" ? "Primary window" : "Secondary window");
    const node = element("div", "", "quota");
    const known =
      typeof window?.usedPercent === "number" &&
      Number.isFinite(window.usedPercent);
    const top = element("div", "", "quota-top");
    top.append(
      element("span", label),
      element(
        "strong",
        known
          ? t("Used {percent}%", {
              percent: Number(window.usedPercent.toFixed(1)),
            })
          : t("Unknown"),
      ),
    );
    node.append(top);
    if (known) {
      const bar = element("progress");
      bar.max = 100;
      bar.value = Math.min(100, Math.max(0, window.usedPercent));
      bar.setAttribute(
        "aria-label",
        `${label}: ${t("Used {percent}%", { percent: window.usedPercent })}`,
      );
      if (window.usedPercent >= 95) bar.className = "high";
      else if (window.usedPercent >= 75) bar.className = "medium";
      node.append(bar);
    } else node.append(element("div", "", "unknown-track"));
    const observed = Object.hasOwn(limits, `${name}ObservedAt`)
      ? limits[`${name}ObservedAt`]
      : limits.observedAt;
    node.append(
      known
        ? timedText("age", observed)
        : element("small", t("No cached quota")),
      timedText("reset", window || null, name),
    );
    return node;
  }
  function notice(message, failed = false) {
    message = t(message);
    $("notice").textContent = message;
    const entry = element(
      "li",
      `${new Date().toLocaleTimeString()} · ${message}`,
    );
    if (failed) entry.className = "error";
    $("activity-list").prepend(entry);
    while ($("activity-list").children.length > 20)
      $("activity-list").lastElementChild.remove();
  }
  async function request(path, payload) {
    const url = new URL(path, location.href);
    if (url.origin !== location.origin)
      throw new Error(
        t("Account manager requests must use this browser origin."),
      );
    const headers =
      payload === undefined ? {} : { "Content-Type": "application/json" };
    if (url.pathname !== "/api/session") {
      if (!state.sessionToken) {
        setPaired(false);
        const error = new Error(
          t("Pair this browser with the account manager first."),
        );
        error.status = 401;
        throw error;
      }
      headers["x-codex-pool-token"] = state.sessionToken;
    }
    let response;
    try {
      response = await fetch(path, {
        method: payload === undefined ? "GET" : "POST",
        credentials: "omit",
        redirect: "error",
        cache: "no-store",
        headers,
        body: payload === undefined ? undefined : JSON.stringify(payload),
        signal: AbortSignal.timeout(45000),
      });
    } catch (cause) {
      throw new Error(
        cause.name === "TimeoutError"
          ? t(
              "The connection timed out. The operation may still be running; check its status before repeating it.",
            )
          : t(
              "Connection interrupted. Check the account manager and read status before repeating an operation.",
            ),
      );
    }
    let data;
    try {
      data = await response.json();
    } catch {
      if (response.ok)
        throw new Error(
          t(
            "The account manager returned an unreadable result. Check status before repeating the operation.",
          ),
        );
      data = {};
    }
    if (!response.ok) {
      const secretOperation =
        payload?.type === "apiAdd" ||
        payload?.type === "apiReplaceKey" ||
        payload?.type === "decisionSave" ||
        payload?.type === "decisionProbe";
      const decisionError =
        payload?.type === "decisionSave" || payload?.type === "decisionProbe"
          ? window.AccountManagerDecision.safeError(data.error)
          : null;
      const error = new Error(
        decisionError ||
          (secretOperation
            ? t(
                "The API credential operation failed (HTTP {status}). Check account status and the provider settings. Re-enter the key to try again.",
                { status: response.status },
              )
            : url.pathname === "/api/session"
              ? t(
                  "Pairing was denied. Open the current pairing URL printed by the manager.",
                )
              : data.error ||
                t("The account manager returned HTTP {status}.", {
                  status: response.status,
                })),
      );
      error.status = response.status;
      if (response.status === 401 || response.status === 403) setPaired(false);
      throw error;
    }
    return data;
  }
  async function operation(payload) {
    const result = await request("/api/operation", payload);
    if (typeof result.message !== "string" || !Object.hasOwn(result, "data"))
      throw new Error(
        t(
          "The operation returned an incomplete result. Check status before repeating it.",
        ),
      );
    if (payload.type === "apiAdd")
      result.message = t(
        "API account saved for manual selection. No generating request was sent.",
      );
    if (payload.type === "apiReplaceKey")
      result.message = t(
        "API key replaced for this profile. No generating request was sent.",
      );
    return result;
  }
  function setPaired(paired) {
    if (state.stopped) return;
    state.paired = paired;
    $("pair-panel").hidden = paired;
    $("dashboard").hidden = !paired;
    $("stop-manager").hidden = !paired;
    $("connection-state").textContent = paired
      ? t("Connected")
      : t("Pairing required");
    if (!paired) {
      state.sessionToken = "";
      try {
        sessionStorage.removeItem(sessionStorageKey);
      } catch {
        /* Memory is cleared even when storage is unavailable. */
      }
      clearTimeout(state.timer);
      window.AccountManagerLifecycle.disconnect();
      if ($("action-dialog").open) $("action-dialog").close();
    } else window.AccountManagerLifecycle.connect(state.sessionToken);
  }
  function scheduleRead() {
    clearTimeout(state.timer);
    if (state.paired && !state.stopped && !document.hidden)
      state.timer = setTimeout(
        () => {
          readInventory().catch(() => {});
        },
        state.readFailed
          ? 30000
          : state.busy ||
              state.inventory?.loginJobs?.some(
                (job) => job.status === "waiting",
              ) ||
              state.inventory?.accounts?.some(
                (account) => account.refresh?.inProgress,
              )
            ? 1000
            : 15000,
      );
  }
  async function readInventory() {
    if (state.stopped) return;
    if (state.reading) return state.reading;
    state.reading = (async () => {
      try {
        const inventory = await request("/api/inventory");
        if (state.stopped) return;
        if (!Array.isArray(inventory.accounts))
          throw new Error(
            t("The account manager returned an invalid inventory."),
          );
        if (!state.preferencesLoaded) {
          state.preferencesLoaded = true;
          try {
            messages.applyPreferences(await request("/api/preferences"));
            $("language").value = messages.language();
          } catch (error) {
            if (error.status === 401) throw error;
            $("language-notice").textContent = t(
              "Saved language could not be read. Account management is still available.",
            );
            $("language-notice").hidden = false;
          }
        }
        guidance.setClock(inventory.hostNow);
        const inventoryKey = JSON.stringify({ ...inventory, hostNow: null });
        const changed = inventoryKey !== state.inventoryKey;
        state.inventory = inventory;
        state.inventoryKey = inventoryKey;
        state.lastRead = Date.now();
        state.readFailed = false;
        setPaired(true);
        if (state.languageDirty) saveLanguage().catch(() => {});
        $("global-error").hidden = true;
        if (changed) renderInventory();
        else updateClocks();
        window.dispatchEvent(
          new CustomEvent("accountmanager:inventory", { detail: inventory }),
        );
        return inventory;
      } catch (error) {
        if (state.stopped) return;
        state.readFailed = true;
        $("global-error").textContent = t(
          "Could not read account status: {message}",
          { message: error.message },
        );
        $("global-error").hidden = false;
        if (error.status !== 401)
          $("connection-state").textContent = t("Connection interrupted");
        throw error;
      } finally {
        state.reading = null;
        scheduleRead();
      }
    })();
    return state.reading;
  }
  async function synchronizeInventory() {
    if (state.reading) await state.reading.catch(() => {});
    return readInventory();
  }
  function accountById(id) {
    return state.inventory?.accounts.find(
      (account) => account.profileId === id,
    );
  }
  function contactAllowed(account) {
    return Boolean(
      account && !account.disabled && account.loginState === "signedIn",
    );
  }
  function renderInventory() {
    const inventory = state.inventory;
    const focused = document.activeElement?.dataset.focusKey;
    window.AccountManagerResetJournal.render(inventory.resetJournals || [], {
      element,
      button,
      confirmOperation,
      busy: state.busy,
      accounts: inventory.accounts,
    });
    window.AccountManagerDecision.render(inventory.decisionAdvisor, {
      element,
      field,
      button,
      openDialog,
      perform,
      operation,
      setBusy,
      showResult,
      busy: state.busy,
    });
    window.AccountManagerRouting.render(inventory.modelRouting, {
      element,
      field,
      button,
      perform,
      operation,
      busy: state.busy,
    });
    window.AccountManagerPrimary.render(
      inventory.primaryLogin,
      confirmOperation,
      { busy: state.busy, hostNow: state.inventory.hostNow },
    );
    const selected = accountById(inventory.activeProfileId);
    const manualApi = inventory.apiSelection?.type === "manual";
    const api = manualApi
      ? inventory.apiAccounts?.find(
          (account) => account.id === inventory.apiSelection.profileId,
        )
      : null;
    $("selection-label").textContent = manualApi
      ? api && !api.disabled && api.hasKey
        ? t("API: {label}", { label: api.label })
        : t("API target unavailable")
      : inventory.paused
        ? t("Subscription accounts paused")
        : selected?.label || t("No subscription account selected");
    $("selection-title").textContent = t(
      manualApi ? "Current selection" : "Last selected subscription",
    );
    $("selection-note").textContent = manualApi
      ? api
        ? t("{model} at {url}. Usage is billed by this provider.", {
            model: api.model,
            url: api.baseUrl,
          })
        : t("Select a subscription account or an enabled API account.")
      : selected && !inventory.paused
        ? guidance.status(selected.availability)
        : t("Select an available account or repair its login below.");
    $("paused-notice").hidden = !inventory.paused;
    updateClocks();
    renderGuidance();
    $("account-count").textContent = `(${inventory.accounts.length})`;
    const settings = inventory.settings || {};
    $("settings-summary").textContent = t(
      "{strategy} · Standby warmup {warmup} · Automatic reset credits {credits}",
      {
        strategy: t(
          settings.rotation_strategy === "earliest_reset"
            ? "Earliest reset"
            : "Priority order",
        ),
        warmup: t(settings.window_warmup === false ? "Off" : "On"),
        credits: t(
          settings.auto_reset_credits === "when_pool_exhausted"
            ? "on when pool exhausted"
            : "Off",
        ),
      },
    );
    $("automatic").hidden = !manualApi && !inventory.paused;
    $("automatic").disabled = state.busy;
    $("refresh-all").disabled =
      state.busy ||
      !inventory.accounts.some(
        (account) => contactAllowed(account) && !account.refresh?.inProgress,
      );
    $("pending-reset").hidden =
      !resetOperations.problem && resetOperations.all().length === 0;
    renderAccounts();
    renderLogins();
    renderApiAccounts();
    if (focused)
      [...$("app").querySelectorAll("[data-focus-key]")]
        .find((node) => node.dataset.focusKey === focused)
        ?.focus();
  }
  function runAccountAction(account, type) {
    if (type === "refresh") return refreshQuota([account.profileId]);
    if (type === "login") {
      if (
        state.inventory.loginJobs?.some(
          (job) =>
            job.profileId === account.profileId && job.status === "waiting",
        )
      )
        return viewLogins();
      return showLogin(account.profileId);
    }
    const enable = type === "enable";
    confirmOperation(
      enable ? "Enable account" : "Use account",
      enable
        ? "Allow this account to participate in the pool again. Login and quota still determine eligibility."
        : "Use this account for subsequent requests. Requests already running keep their current identity.",
      enable
        ? { type: "update", profileId: account.profileId, disabled: false }
        : { type: "use", profileId: account.profileId },
      [
        [t("Account"), account.label],
        [t("Profile ID"), account.profileId],
      ],
    );
  }
  function viewLogins() {
    $("login-section").scrollIntoView({ block: "start" });
    $("login-title").focus();
  }
  function renderGuidance() {
    const { counts, message, next } = guidance.summary(state.inventory);
    $("pool-counts").replaceChildren(
      ...[
        [t("Eligible locally"), counts.eligible],
        [t("Login needed"), counts.login],
        [t("Cooling down"), counts.cooling],
      ].map(([label, count]) => {
        const item = element("span");
        item.append(
          element("strong", count),
          document.createTextNode(` ${t(label)}`),
        );
        return item;
      }),
    );
    $("guidance-message").textContent = message;
    $("guidance-action").replaceChildren();
    if (next) {
      const action = button(
        next.label,
        () => {
          if (next.type === "automatic") $("automatic").click();
          else if (next.type === "add") showLogin();
          else if (next.type === "refresh") refreshQuota();
          else if (next.type === "loginActivity") viewLogins();
          else if (next.type === "login") showLogin(next.profileId);
          else showAccount(next.profileId);
        },
        "primary",
      );
      action.disabled = state.busy;
      action.dataset.focusKey = "guidance:primary";
      $("guidance-action").append(action);
    }
  }
  function renderAccounts() {
    const focused = document.activeElement?.dataset.focusKey;
    const query = $("account-search").value.trim().toLocaleLowerCase();
    const filter = $("account-filter").value;
    const accounts = state.inventory.accounts.filter(
      (account) =>
        (filter === "all" ||
          account.availability === filter ||
          (filter === "needsLogin" &&
            account.loginState !== "signedIn" &&
            !account.disabled)) &&
        [account.label, account.email, account.profileId].some((value) =>
          value?.toLocaleLowerCase().includes(query),
        ),
    );
    const pages = Math.max(1, Math.ceil(accounts.length / pageSize));
    state.page = Math.min(state.page, pages - 1);
    $("account-rows").replaceChildren();
    for (const account of accounts.slice(
      state.page * pageSize,
      (state.page + 1) * pageSize,
    )) {
      const row = element("tr");
      const identity = element("td");
      identity.append(element("p", account.label, "account-name"));
      if (
        account.profileId === state.inventory.activeProfileId &&
        !state.inventory.paused &&
        state.inventory.apiSelection?.type !== "manual"
      )
        identity.append(element("span", t("Selected"), "badge active"));
      if (state.inventory.primaryLogin?.profileId === account.profileId)
        identity.append(element("span", t("Host sign-in"), "badge"));
      identity.append(
        element(
          "p",
          [account.plan, account.email === account.label ? null : account.email]
            .filter(Boolean)
            .join(" · ") || t("Subscription account"),
          "account-meta muted",
        ),
      );
      const availability = element("td");
      availability.append(
        element(
          "span",
          guidance.status(account.availability),
          `badge ${statusNames[account.availability] ? account.availability : ""}`,
        ),
      );
      if (account.loginState === "pending")
        availability.append(element("small", t("Login pending")));
      if (account.cooldownUntil)
        availability.append(
          element(
            "small",
            t("Retry after {date}", { date: date(account.cooldownUntil) }),
          ),
        );
      if (account.refresh?.inProgress)
        availability.append(element("small", t("Checking backend quota…")));
      else if (account.refresh && !account.refresh.succeeded)
        availability.append(
          element("small", t("Refresh failed; cached quota retained")),
        );
      const primary = element("td");
      primary.append(quota(account, "primary"));
      const secondary = element("td");
      secondary.append(quota(account, "secondary"));
      const actions = element("td");
      const buttons = element("div", "", "row-actions");
      const manage = button(
        "Details & actions",
        () => showAccount(account.profileId),
        "link",
      );
      manage.dataset.focusKey = `${account.profileId}:details`;
      manage.setAttribute(
        "aria-label",
        t("Details and actions for {label}", { label: account.label }),
      );
      const next = guidance.action(account);
      const primaryAction = button(
        next.label,
        () => runAccountAction(account, next.type),
        "link",
      );
      primaryAction.disabled =
        state.busy || Boolean(account.refresh?.inProgress);
      primaryAction.dataset.focusKey = `${account.profileId}:primary`;
      primaryAction.setAttribute(
        "aria-label",
        `${next.label}: ${account.label}`,
      );
      const reset = button(
        "Use reset credit",
        () => loadCredits(account.profileId),
        "quick-reset",
      );
      reset.dataset.focusKey = `${account.profileId}:credit`;
      reset.disabled = state.busy || !contactAllowed(account);
      reset.setAttribute(
        "aria-label",
        t("Use reset credit for {label}", { label: account.label }),
      );
      if (!contactAllowed(account))
        reset.title = t("Enable this account and complete login first.");
      buttons.append(primaryAction, reset, manage);
      if (!["ready", "coolingDown"].includes(account.availability))
        actions.append(
          element("small", guidance.reason(account), "action-reason"),
        );
      actions.append(buttons);
      row.append(identity, availability, primary, secondary, actions);
      $("account-rows").append(row);
    }
    $("account-empty").hidden = accounts.length !== 0;
    $("empty-message").textContent = state.inventory.accounts.length
      ? t("Change your search or availability filter.")
      : t("Add a subscription account to start managing this pool.");
    $("page-label").textContent = accounts.length
      ? t("{first}–{last} of {count}", {
          first: state.page * pageSize + 1,
          last: Math.min((state.page + 1) * pageSize, accounts.length),
          count: accounts.length,
        })
      : t("0 accounts");
    $("previous-page").disabled = state.page === 0;
    $("next-page").disabled = state.page + 1 >= pages;
    if (focused)
      [...$("account-rows").querySelectorAll("button")]
        .find((node) => node.dataset.focusKey === focused)
        ?.focus();
  }
  function renderLogins() {
    const jobs = state.inventory.loginJobs || [];
    $("login-section").hidden = jobs.length === 0;
    $("login-jobs").replaceChildren();
    for (const job of jobs) {
      const node = element("article", "", "login-job");
      node.append(
        element(
          "h3",
          accountById(job.profileId)?.label ||
            job.profileId ||
            t("New account"),
        ),
        element(
          "span",
          guidance.status(job.status),
          `badge ${statusNames[job.status] ? job.status : ""}`,
        ),
        element("p", t(job.message), "muted"),
      );
      if (job.userCode) node.append(element("code", job.userCode, "user-code"));
      const actions = element("div", "", "actions");
      if (job.verificationUrl) {
        try {
          const url = new URL(job.verificationUrl);
          if (url.protocol === "https:") {
            const link = element("a", t("Open verification link"));
            link.href = url.href;
            link.target = "_blank";
            link.rel = "noopener noreferrer";
            link.dataset.focusKey = `${job.operationId}:verification`;
            actions.append(link);
          }
        } catch {
          /* Invalid links are not opened. */
        }
      }
      if (job.userCode)
        actions.append(
          button("Copy code", async () => {
            try {
              await navigator.clipboard.writeText(job.userCode);
              notice("Verification code copied.");
            } catch {
              notice("Select and copy the verification code above.", true);
            }
          }),
        );
      if (job.status === "waiting")
        actions.append(
          button("Cancel login", () =>
            confirmOperation(
              "Cancel login",
              "Cancel this device verification flow. A new unfinished profile will be removed; existing accounts are retained.",
              { type: "cancelLogin", operationId: job.operationId },
              [
                [
                  t("Account"),
                  accountById(job.profileId)?.label || job.profileId,
                ],
                [t("Operation ID"), job.operationId],
              ],
            ),
          ),
        );
      for (const action of actions.querySelectorAll("button"))
        action.dataset.focusKey = `${job.operationId}:${action.textContent}`;
      node.append(actions);
      $("login-jobs").append(node);
    }
  }
  function setBusy(busy) {
    state.busy = busy;
    $("dialog-fields").disabled = busy;
    $("dialog-cancel").disabled = busy;
    $("dialog-submit").disabled =
      busy ||
      dialogCompleted ||
      $("dialog-submit").dataset.unavailable === "true";
    if (busy) $("dialog-submit").textContent = t("Working…");
    else if (dialogTask)
      $("dialog-submit").textContent = $("dialog-submit").dataset.label;
    $("action-form").setAttribute("aria-busy", String(busy));
    $("language").disabled =
      busy || state.preferenceSaving || $("action-dialog").open;
    $("stop-manager").disabled = busy;
    scheduleRead();
    for (const id of [
      "add-account",
      "edit-settings",
      "review-reset",
      "add-api-account",
      "edit-api-fallback",
    ])
      $(id).disabled = busy;
    if (state.inventory) renderInventory();
  }
  function showResult(message, failed = false) {
    $("dialog-result").textContent = t(message);
    $("dialog-result").className = `message ${failed ? "error" : "success"}`;
    $("dialog-result").hidden = false;
    $("dialog-cancel").textContent = t(dialogCompleted ? "Close" : "Cancel");
  }
  function openDialog(
    title,
    description,
    build,
    submit = null,
    submitLabel = t("Confirm"),
  ) {
    if (state.busy) return;
    if (!$("action-dialog").open) {
      returnFocus = document.activeElement;
      returnFocusKey = returnFocus?.dataset.focusKey;
    }
    dialogTask = submit;
    dialogAccountId = null;
    dialogCompleted = false;
    $("dialog-title").textContent = title;
    $("dialog-description").textContent = t(description);
    $("dialog-body").replaceChildren();
    $("dialog-result").hidden = true;
    $("dialog-submit").hidden = !submit;
    $("dialog-submit").disabled = false;
    $("dialog-submit").textContent = t(submitLabel);
    $("dialog-submit").dataset.label = t(submitLabel);
    delete $("dialog-submit").dataset.unavailable;
    $("dialog-cancel").textContent = t(submit ? "Cancel" : "Close");
    build($("dialog-body"));
    if (!$("action-dialog").open) $("action-dialog").showModal();
    $("language").disabled = true;
    $("dialog-title").focus();
  }
  async function perform(payload) {
    if (state.busy)
      throw new Error(t("Wait for the current operation to finish."));
    setBusy(true);
    try {
      const result = await operation(payload);
      dialogCompleted = true;
      if ($("action-dialog").open)
        showResult(result.message || "Operation completed.");
      notice(result.message || "Operation completed.");
      try {
        await synchronizeInventory();
      } catch {
        const message = t(
          "Changes saved, but status could not be synchronized. Read host status before repeating the operation.",
        );
        notice(message, true);
        if ($("action-dialog").open) showResult(message, true);
      }
      return result;
    } finally {
      setBusy(false);
    }
  }
  function confirmOperation(title, description, payload, items = []) {
    openDialog(
      t(title),
      description,
      (body) => body.append(definitions(items)),
      () => perform(payload),
      title,
    );
  }
  async function refreshQuota(ids = null) {
    if (state.busy) return;
    const detailId = dialogAccountId;
    setBusy(true);
    try {
      notice("Checking backend quota…");
      const targets =
        ids ||
        state.inventory.accounts
          .filter(contactAllowed)
          .map((account) => account.profileId);
      let checked = 0;
      let failed = 0;
      let succeeded = 0;
      let pending = 0;
      for (let offset = 0; offset < targets.length; offset += 4) {
        notice(
          t("Checking quota {checked}/{total}…", {
            checked,
            total: targets.length,
          }),
        );
        const batch = targets.slice(offset, offset + 4);
        await operation({ type: "refresh", profileIds: batch });
        await synchronizeInventory();
        checked += batch.length;
        for (const id of batch) {
          const status = accountById(id)?.refresh;
          if (status?.inProgress || !status) pending++;
          else if (status.succeeded) succeeded++;
          else failed++;
        }
      }
      notice(
        t(
          "Quota check complete: {succeeded} succeeded, {failed} failed. Failed accounts keep their cached quota.",
          {
            succeeded,
            failed,
          },
        ),
        failed > 0,
      );
      if (pending)
        notice(
          t(
            "{count} quota checks are still running or unconfirmed. Read host status before repeating them.",
            { count: pending },
          ),
        );
      if (detailId && accountById(detailId)) {
        setBusy(false);
        showAccount(detailId);
      }
    } catch (error) {
      notice(
        t("Quota refresh failed: {message}", { message: error.message }),
        true,
      );
    } finally {
      setBusy(false);
    }
  }
  function showAccount(id) {
    const account = accountById(id);
    if (!account) {
      notice(
        "This profile no longer exists. Read the current account list.",
        true,
      );
      return;
    }
    openDialog(
      account.label,
      "Manage this exact profile. Selection and account changes are shared with other clients using this Codex home.",
      (body) => {
        body.append(
          definitions([
            ["Login", guidance.status(account.loginState)],
            ["Availability", guidance.status(account.availability)],
            ["Reset credits", account.resetCreditCount],
          ]),
        );
        const quotas = element("div", "", "detail-quotas");
        quotas.append(quota(account, "primary"), quota(account, "secondary"));
        body.append(
          quotas,
          element("p", guidance.reason(account), "disclosure"),
        );
        const next = guidance.action(account);
        const mainAction = button(
          next.label,
          () => runAccountAction(account, next.type),
          "primary",
        );
        mainAction.disabled = Boolean(account.refresh?.inProgress);
        body.append(mainAction);
        if (account.refresh)
          body.append(
            element(
              "p",
              t("Last refresh {date}: {message}", {
                date: date(account.refresh.attemptedAt),
                message: t(account.refresh.message),
              }),
              "disclosure",
            ),
          );
        const actions = element("div", "", "actions detail-actions");
        const hostSignIn = button("Use for host sign-in", () =>
          confirmOperation(
            "Use for host sign-in",
            "Use this exact subscription profile for host sign-in. An enabled Remote service reconnects; a disabled service stays disabled. The phone must use the matching account and workspace and may need pairing again. Inference selection is unchanged.",
            { type: "primaryUse", profileId: id },
            [
              ["Account", account.label],
              ["Profile ID", id],
              ["Email", account.email],
            ],
          ),
        );
        hostSignIn.disabled = account.loginState !== "signedIn";
        const retry = button("Retry without credit", () =>
          confirmOperation(
            "Retry without credit",
            "Clear this account's local quota cooldown for one real request. This selects the account; the backend may still reject it. No reset credit is consumed.",
            { type: "retry", profileId: id },
            [
              [t("Account"), account.label],
              [t("Profile ID"), id],
            ],
          ),
        );
        retry.disabled = !contactAllowed(account);
        const credits = button("Use reset credit", () => loadCredits(id));
        credits.disabled = !contactAllowed(account);
        const refresh = button("Refresh quota", () => refreshQuota([id]));
        refresh.disabled = !contactAllowed(account);
        const recovery = element("div", "", "actions");
        recovery.append(refresh, retry, credits);
        body.append(element("h3", t("Recovery options")), recovery);
        actions.append(
          button(
            account.loginState === "signedIn"
              ? "Sign in again"
              : "Complete login",
            () => showLogin(id),
          ),
          hostSignIn,
          button("Edit label & priority", () => showEdit(id)),
          button(account.disabled ? "Enable account" : "Disable account", () =>
            confirmOperation(
              account.disabled ? "Enable account" : "Disable account",
              account.disabled
                ? "Allow this account to participate in the pool again. Login and quota still determine eligibility."
                : "Exclude this account from future pool selection. This does not cancel a request already in progress.",
              { type: "update", profileId: id, disabled: !account.disabled },
              [
                [t("Account"), account.label],
                [t("Profile ID"), id],
              ],
            ),
          ),
          button("Remove from pool", () => showRemove(id, true), "danger"),
          ...(id === "legacy-root"
            ? []
            : [
                button(
                  "Remove and delete sign-in",
                  () => showRemove(id, false),
                  "danger",
                ),
              ]),
        );
        body.append(
          disclosure(
            "Standby warmup",
            window.AccountManagerWarmup.render(account, {
              element,
              definitions,
              date,
            }),
          ),
          element("h3", t("Account actions")),
          actions,
          disclosure(
            "Technical details",
            definitions([
              ["Profile ID", id],
              ["Email", account.email],
              ["Plan", account.plan],
              ["Priority", account.priority],
              ["Backend reset", date(account.backendResetsAt)],
            ]),
          ),
        );
      },
    );
    dialogAccountId = id;
  }
  function showEdit(id) {
    const account = accountById(id);
    let label, priority;
    openDialog(
      t("Edit account"),
      "A lower priority number is preferred. A blank label uses the account email, then the profile ID.",
      (body) => {
        body.append(definitions([["Profile ID", id]]));
        label = field(
          body,
          "label",
          "Label",
          "text",
          account.customLabel ?? "",
          "Up to 80 characters.",
          { maxLength: 80 },
        );
        priority = field(
          body,
          "priority",
          "Priority",
          "number",
          account.priority,
          "Lower numbers are selected first in priority order.",
          { min: 0, max: 4294967295, step: 1, required: true },
        );
      },
      () => {
        const nextLabel = label.value.trim();
        return perform({
          type: "update",
          profileId: id,
          label:
            nextLabel === (account.customLabel ?? "").trim() ? null : nextLabel,
          priority: Number(priority.value),
        });
      },
      "Save account",
    );
  }
  function showRemove(id, keepCredentials) {
    const account = accountById(id);
    confirmOperation(
      keepCredentials ? "Remove from pool" : "Remove and delete sign-in",
      keepCredentials
        ? "Remove this account from the pool. Keep its saved sign-in credentials."
        : "Remove this account and delete its saved sign-in credentials. Adding it again requires login. Server revocation is attempted.",
      { type: "remove", profileId: id, keepCredentials },
      [
        ["Account", account.label],
        ["Email", account.email],
        ["Profile ID", id],
      ],
    );
  }
  function showLogin(id = null) {
    let label;
    openDialog(
      id ? t("Sign in again") : t("Add account"),
      id
        ? "Start device verification for this exact profile. Keep the verification link open until login completes."
        : "Create a new subscription account profile, then sign in using a verification code.",
      (body) => {
        if (id)
          body.append(
            definitions([
              ["Account", accountById(id)?.label],
              ["Profile ID", id],
            ]),
          );
        else
          label = field(
            body,
            "newLabel",
            "Account label",
            "text",
            "",
            "Optional. Choose a label you can recognize on every device.",
            { maxLength: 80 },
          );
      },
      async () => {
        const result = await perform({
          type: "login",
          profileId: id,
          label: label?.value.trim() || null,
        });
        if (result.data?.operationId) {
          $("action-dialog").close();
          $("login-section").scrollIntoView({ block: "nearest" });
        }
      },
      "Start verification",
    );
  }
  function creditValidity(credit) {
    if (
      typeof credit.id !== "string" ||
      !credit.id ||
      credit.id.length > 256 ||
      typeof credit.resetType !== "string" ||
      !credit.resetType
    )
      return {
        eligible: false,
        label: t("Credit identity or scope could not be verified"),
      };
    if (credit.resetType !== "codex_rate_limits")
      return {
        eligible: false,
        label: t("Unsupported quota scope; no credit will be consumed"),
      };
    if (credit.status !== "available")
      return {
        eligible: false,
        label: t("Unavailable ({status})", {
          status: credit.status || t("unknown status"),
        }),
      };
    if (credit.expiresAt === null || credit.expiresAt === undefined)
      return { eligible: true, label: t("Available; no expiry reported") };
    const expiry = Date.parse(credit.expiresAt);
    if (!Number.isFinite(expiry))
      return { eligible: false, label: t("Expiry could not be verified") };
    return expiry > guidance.now() * 1000
      ? { eligible: true, label: t("Available; not expired") }
      : { eligible: false, label: t("Expired") };
  }
  function creditDetails(credit) {
    return [
      [t("Credit ID"), credit.id],
      [t("Backend status"), credit.status],
      [t("Validity"), creditValidity(credit).label],
      [
        t("Expires"),
        credit.expiresAt
          ? `${date(credit.expiresAt)} (${credit.expiresAt})`
          : t("Not reported"),
      ],
      [t("Scope"), credit.resetType],
      [t("Granted"), date(credit.grantedAt)],
    ];
  }
  async function loadCredits(id) {
    openDialog(
      t("Reset credits"),
      "Reading credit details for the selected profile…",
      (body) =>
        body.append(
          definitions([
            ["Account", accountById(id)?.label],
            ["Profile ID", id],
          ]),
        ),
    );
    setBusy(true);
    try {
      const result = await operation({ type: "credits", profileId: id });
      if (result.data?.profileId !== id || !Array.isArray(result.data.credits))
        throw new Error(
          t("Credit details did not match the selected profile."),
        );
      const readAt = Math.floor(guidance.now());
      await synchronizeInventory();
      setBusy(false);
      const recovered = result.data.pendingResetCredit;
      if (
        recovered !== null &&
        (!verifiedResetTuple(recovered) ||
          recovered.ownerKey !== result.data.resetOwnerKey)
      )
        throw new Error(
          t(
            "The pending reset could not be verified. Reload credits before retrying.",
          ),
        );
      if (recovered) {
        const existing = resetOperations.current(
          id,
          recovered.ownerKey,
          recovered.idempotencyKey,
        );
        resetOperations.recover({
          profileId: id,
          creditId: recovered.creditId,
          idempotencyKey: recovered.idempotencyKey,
          ownerKey: recovered.ownerKey,
          startedAt: existing?.startedAt ?? readAt,
        });
        $("pending-reset").hidden = false;
      }
      const pending = resetOperations.current(
        id,
        result.data.resetOwnerKey,
        recovered?.idempotencyKey,
      );
      const eligible = result.data.credits
        .filter(
          (credit) =>
            creditValidity(credit).eligible && contactAllowed(accountById(id)),
        )
        .sort((left, right) => {
          const expiry = (credit) =>
            credit.expiresAt ? Date.parse(credit.expiresAt) : Infinity;
          return (
            expiry(left) - expiry(right) ||
            String(left.id).localeCompare(String(right.id))
          );
        });
      const selected = pending
        ? pending.profileId === id &&
          verifiedResetTuple(pending) &&
          pending.ownerKey === result.data.resetOwnerKey &&
          (result.data.credits.find(
            (credit) => credit.id === pending.creditId,
          ) || {
            id: pending.creditId,
            title: t("Previous reset credit"),
            status: "unknown",
            resetType: "codex_rate_limits",
            expiresAt: null,
          })
        : eligible[0];
      const changedOwner = resetOperations
        .all()
        .some(
          (record) =>
            record.profileId === id &&
            record.ownerKey &&
            record.ownerKey !== result.data.resetOwnerKey,
        );
      if (selected && (!changedOwner || recovered))
        showRedemption(id, selected, { data: result.data, readAt });
      else showCredits(id, result.data, readAt);
      if (
        result.data.inventoryError !== null &&
        result.data.inventoryError !== undefined
      )
        notice(
          "Credit inventory is unavailable. The available credit count is unknown.",
          true,
        );
      else notice(result.message || "Reset credits loaded.");
    } catch (error) {
      setBusy(false);
      showResult(error.message, true);
    }
  }
  function creditInventoryWarning(data) {
    if (data?.inventoryError === null || data?.inventoryError === undefined)
      return null;
    const warning = element("div", "", "message warning");
    warning.append(
      element(
        "p",
        t(
          "Credit inventory is unavailable. The available credit count is unknown.",
        ),
      ),
      element(
        "p",
        t(
          "The original operation is preserved. Retrying uses the same account, credit, and operation ID.",
        ),
      ),
    );
    return warning;
  }
  function showCredits(id, data, readAt) {
    const account = accountById(id);
    const pending = resetOperations.current(
      id,
      data.resetOwnerKey,
      data.pendingResetCredit?.idempotencyKey,
    );
    const inventoryWarning = creditInventoryWarning(data);
    openDialog(
      t("Choose a reset credit"),
      "Choose a credit. Nothing is consumed until you confirm.",
      (body) => {
        body.append(
          definitions([
            ["Account", account?.label],
            ["Profile ID", id],
            [
              t("Eligible credits"),
              inventoryWarning
                ? t("Unknown")
                : data.credits.filter(
                    (credit) => creditValidity(credit).eligible,
                  ).length,
            ],
          ]),
        );
        if (inventoryWarning) body.append(inventoryWarning);
        for (const original of resetOperations
          .all()
          .filter(
            (record) =>
              record.profileId === id &&
              record.ownerKey &&
              (record.ownerKey !== data.resetOwnerKey ||
                (data.pendingResetCredit &&
                  record.idempotencyKey !==
                    data.pendingResetCredit.idempotencyKey)),
          )) {
          body.append(
            element(
              "p",
              t(
                "An earlier original operation is retained separately. Review its account and outcome before clearing this browser's retry record.",
              ),
              "message warning",
            ),
          );
          appendResetReview(body, original, {
            ownerChanged: original.ownerKey !== data.resetOwnerKey,
            serverDifferent: true,
            readAt,
          });
        }
        if (!contactAllowed(account))
          body.append(
            element(
              "p",
              t(
                "Enable this account and complete login before consuming a credit.",
              ),
              "message warning",
            ),
          );
        if (pending)
          body.append(
            element(
              "p",
              t(
                "An earlier operation is unconfirmed. You may retry its same credit with the same operation ID after checking the details below.",
              ),
              "disclosure",
            ),
          );
        if (!inventoryWarning && !data.credits.length)
          body.append(
            element(
              "p",
              t("No reset credits were returned for this account."),
              "disclosure",
            ),
          );
        for (const credit of data.credits) {
          const choice = element("div", "", "credit-choice");
          const select = button(
            "Use this credit",
            () => showRedemption(id, credit, { data, readAt }),
            "primary",
          );
          select.disabled =
            Boolean(inventoryWarning) ||
            resetOperations.problem ||
            !contactAllowed(account) ||
            !creditValidity(credit).eligible ||
            Boolean(
              pending &&
                (!verifiedResetTuple(pending) ||
                  pending.profileId !== id ||
                  pending.creditId !== credit.id),
            );
          if (select.disabled) select.dataset.unavailable = "true";
          const content = element("div");
          content.append(element("strong", credit.title || t("Reset credit")));
          if (credit.description)
            content.append(
              disclosure("About this credit", element("p", credit.description)),
            );
          content.append(
            definitions([
              ["Validity", creditValidity(credit).label],
              ["Expires", date(credit.expiresAt)],
              ["Scope", guidance.creditScope(credit.resetType)],
            ]),
            disclosure("Technical details", definitions(creditDetails(credit))),
          );
          choice.append(content, select);
          body.append(choice);
        }
        $("dialog-submit").disabled = true;
        $("dialog-submit").dataset.unavailable = "true";
        if (
          !inventoryWarning &&
          !data.credits.some(
            (credit) =>
              creditValidity(credit).eligible &&
              contactAllowed(account) &&
              (!pending ||
                (verifiedResetTuple(pending) &&
                  pending.profileId === id &&
                  pending.creditId === credit.id)),
          )
        )
          body.append(
            element(
              "p",
              t(
                "No eligible reset credits. Refresh quota to check for recovery, or wait for a natural reset; neither action spends a credit.",
              ),
              "message warning",
            ),
          );
        const prior = data.credits.find(
          (credit) => credit.id === pending?.creditId,
        );
        if (
          pending?.profileId === id &&
          !inventoryWarning &&
          !data.pendingResetCredit &&
          (!verifiedResetTuple(pending) ||
            !prior ||
            !creditValidity(prior).eligible)
        )
          body.append(
            button("Clear reviewed operation", () =>
              confirmOperationClear(pending, { credit: prior, readAt }),
            ),
          );
      },
    );
  }
  const verifiedResetTuple = window.AccountManagerResetOperations.verified;
  function saveRedemption(value) {
    resetOperations.put(value);
    $("pending-reset").hidden = false;
  }
  function clearRedemption(value) {
    resetOperations.remove(value);
    $("pending-reset").hidden = resetOperations.all().length === 0;
  }
  function appendResetReview(body, pending, evidence) {
    const journal = state.inventory?.resetJournals?.find(
      (record) =>
        record.manual?.ownerKey === pending.ownerKey &&
        record.manual.idempotencyKey === pending.idempotencyKey,
    );
    if (journal)
      body.append(
        button("Review original server operation", () =>
          window.AccountManagerResetJournal.review(journal, {
            confirmOperation,
          }),
        ),
      );
    if (
      evidence.profileMissing ||
      evidence.ownerChanged ||
      evidence.serverDifferent ||
      (evidence.serverHasNoPending && !journal) ||
      !verifiedResetTuple(pending)
    )
      body.append(
        button("Clear reviewed operation", () =>
          confirmOperationClear(pending, evidence),
        ),
      );
  }
  function showRedemption(id, credit, context = null) {
    const original = resetOperations.current(
      id,
      context?.data?.resetOwnerKey,
      context?.data?.pendingResetCredit?.idempotencyKey,
    );
    const inventoryWarning = creditInventoryWarning(context?.data);
    openDialog(
      t("Use this reset credit"),
      inventoryWarning
        ? "Retry the original reset operation. Its outcome may remain unconfirmed."
        : "Confirm to consume one credit for this account.",
      (body) => {
        body.append(
          definitions([
            ["Account", accountById(id)?.label],
            ["Profile ID", id],
            ["Credit", credit.title || t("Reset credit")],
            ["Expires", date(credit.expiresAt)],
            ["Scope", guidance.creditScope(credit.resetType)],
          ]),
        );
        if (inventoryWarning) body.append(inventoryWarning);
        if (original)
          body.append(
            definitions([["Existing operation ID", original.idempotencyKey]]),
          );
        if (original && context)
          appendResetReview(body, original, {
            serverHasNoPending:
              !inventoryWarning &&
              context.data.pendingResetCredit === null &&
              context.data.resetOwnerKey === original.ownerKey,
            credit,
            readAt: context.readAt,
          });
        for (const prior of resetOperations
          .all()
          .filter(
            (record) =>
              record.profileId === id &&
              record.ownerKey &&
              (record.ownerKey !== context?.data?.resetOwnerKey ||
                (context?.data?.pendingResetCredit &&
                  record.idempotencyKey !==
                    context.data.pendingResetCredit.idempotencyKey)),
          )) {
          body.append(
            element(
              "p",
              t(
                "An earlier original operation is retained separately. Review its account and outcome before clearing this browser's retry record.",
              ),
              "message warning",
            ),
          );
          appendResetReview(body, prior, {
            ownerChanged: prior.ownerKey !== context?.data?.resetOwnerKey,
            serverDifferent: true,
            readAt: context?.readAt,
          });
        }
        if (context && !original) {
          body.append(
            element(
              "p",
              t("Earliest-expiring available credit selected."),
              "muted",
            ),
          );
          if (
            context.data.credits.filter((item) => creditValidity(item).eligible)
              .length > 1
          )
            body.append(
              button(
                "Choose another credit",
                () => showCredits(id, context.data, context.readAt),
                "link",
              ),
            );
        }
        body.append(
          disclosure("Technical details", definitions(creditDetails(credit))),
        );
      },
      async () => {
        const replay =
          original?.profileId === id && original.creditId === credit.id;
        const owner = replay ? original.ownerKey : context?.data?.resetOwnerKey;
        if (
          typeof owner !== "string" ||
          !/^[a-f0-9]{64}$/.test(owner) ||
          context?.data?.resetOwnerKey !== owner
        )
          throw new Error(
            t(
              "Account changed since confirmation. Reload credits for the original account before retrying.",
            ),
          );
        if (
          (!replay && (inventoryWarning || !creditValidity(credit).eligible)) ||
          !contactAllowed(accountById(id))
        )
          throw new Error(
            t(
              "This credit or account is no longer eligible. Read the current details first.",
            ),
          );
        let pending = resetOperations.current(
          id,
          owner,
          original?.idempotencyKey,
        );
        if (
          original &&
          (!pending ||
            pending.idempotencyKey !== original.idempotencyKey ||
            pending.creditId !== original.creditId ||
            pending.ownerKey !== original.ownerKey)
        )
          throw new Error(
            t(
              "The pending reset operation changed. Review its current record again.",
            ),
          );
        if (
          pending &&
          (pending.profileId !== id || pending.creditId !== credit.id)
        )
          throw new Error(
            t(
              "Review the earlier unconfirmed operation before starting another reset.",
            ),
          );
        if (!pending) {
          if (!crypto.randomUUID)
            throw new Error(
              t(
                "Open this manager through HTTPS or localhost before consuming a credit.",
              ),
            );
          pending = {
            profileId: id,
            creditId: credit.id,
            idempotencyKey: crypto.randomUUID(),
            ownerKey: owner,
            startedAt: Math.floor(Date.now() / 1000),
          };
          saveRedemption(pending);
        }
        try {
          await perform({
            type: "redeem",
            profileId: id,
            creditId: credit.id,
            idempotencyKey: pending.idempotencyKey,
            expectedOwnerKey: pending.ownerKey,
          });
          clearRedemption(pending);
        } catch (error) {
          throw new Error(
            t(
              "{message} The outcome remains unconfirmed. Refresh quota and inspect this credit before explicitly retrying the same operation.",
              { message: error.message },
            ),
          );
        }
      },
      original ? "Retry same credit operation" : "Consume selected credit",
    );
  }
  function confirmOperationClear(pending, evidence) {
    let acknowledged;
    const credit = evidence.credit;
    openDialog(
      t("Clear reviewed reset operation"),
      evidence.profileMissing
        ? "A fresh account read shows the original profile is no longer enrolled. Its redemption outcome cannot be checked here. Clear only this browser's retry record after reviewing the uncertainty; no credit will be consumed."
        : !verifiedResetTuple(pending)
          ? "This legacy retry record has no verified account binding. Review the original outcome before clearing this local record. No credit is consumed."
          : evidence.ownerChanged
            ? "The profile now belongs to another account identity. Clear only this browser's retry record after independently reviewing the original outcome. Server spending restrictions remain until its record is explicitly reviewed. No credit is consumed."
            : evidence.serverDifferent
              ? "The server reports a different original operation. This browser's older record is retained separately. Clear only its browser retry record after independently checking the earlier outcome. Server spending restrictions remain unchanged. No credit is consumed."
              : evidence.serverHasNoPending
                ? "The latest account read reports no unresolved original reset. This does not prove whether it consumed a credit. Clear only this browser's retained retry record after independently reviewing its outcome. Server history and spending restrictions remain unchanged. No credit is consumed."
                : "The latest successful credit read shows this credit is absent or unavailable. This does not prove whether the earlier operation consumed it. Clear only this browser's retry record; no credit will be consumed.",
      (body) => {
        body.append(
          definitions([
            ["Profile ID", pending.profileId],
            ["Credit ID", pending.creditId],
            [
              "Review evidence",
              evidence.profileMissing
                ? t("Profile absent from current inventory")
                : evidence.ownerChanged
                  ? t("Profile now belongs to another account identity")
                  : evidence.serverDifferent
                    ? t("Server reports a different original operation")
                    : evidence.serverHasNoPending
                      ? t(
                          "No unresolved original reset returned by the latest account read",
                        )
                      : !verifiedResetTuple(pending)
                        ? t("Retry record has no verified account binding")
                        : credit
                          ? t("Credit unavailable ({status})", {
                              status: credit.status,
                            })
                          : t("Credit absent from latest returned list"),
            ],
            [
              "Validity",
              credit ? creditValidity(credit).label : t("Outcome unconfirmed"),
            ],
            ["Successful read", date(evidence.readAt)],
            ["Operation ID", pending.idempotencyKey],
          ]),
        );
        acknowledged = field(
          body,
          "clearAcknowledgement",
          "I reviewed this operation and accept that its outcome may remain unknown. Clear its retry record.",
          "checkbox",
          false,
          "Clearing only removes this browser's retry record. Server spending restrictions remain until the original operation is completed or explicitly reviewed. No credit is consumed.",
          { required: true },
        );
      },
      () => {
        if (!acknowledged.checked)
          throw new Error(
            t(
              "Acknowledge the unconfirmed outcome before clearing this record.",
            ),
          );
        if (
          !resetOperations
            .all()
            .some(
              (record) =>
                record.idempotencyKey === pending.idempotencyKey &&
                record.ownerKey === pending.ownerKey &&
                record.creditId === pending.creditId,
            )
        )
          throw new Error(
            t(
              "The pending reset operation changed. Review its current record again.",
            ),
          );
        if (evidence.profileMissing && accountById(pending.profileId))
          throw new Error(
            t(
              "This profile has reappeared. Inspect its current credits before clearing the record.",
            ),
          );
        clearRedemption(pending);
        dialogCompleted = true;
        showResult("Reviewed operation record cleared.");
        $("dialog-submit").disabled = true;
      },
      "Clear reviewed operation",
    );
  }
  async function showPendingReset() {
    if (resetOperations.problem) {
      openDialog(
        t("Unconfirmed reset operation"),
        "Stored reset operations could not be verified. Review the original records before starting another reset.",
        (body) =>
          body.append(
            element(
              "p",
              t(
                "Use the interrupted reset records below to review server operations. Browser storage must be repaired before new reset operations can be saved.",
              ),
            ),
          ),
      );
      return;
    }
    const pending = resetOperations.all();
    if (!pending.length) return;
    setBusy(true);
    try {
      await readInventory();
    } catch (error) {
      setBusy(false);
      notice(error.message, true);
      return;
    }
    setBusy(false);
    openDialog(
      t("Unconfirmed reset operations"),
      "Each original account and operation is retained separately. Inspect its current quota and credit history before explicitly retrying or reviewing it.",
      (body) => {
        for (const record of pending) {
          const account = accountById(record.profileId);
          const section = element("div", "", "credit-choice");
          section.append(
            definitions([
              ["Account", account?.label || t("Profile no longer enrolled")],
              ["Profile ID", record.profileId],
              ["Credit ID", record.creditId],
              ["Operation ID", record.idempotencyKey],
              ["Started", date(record.startedAt)],
            ]),
          );
          const actions = element("div", "", "actions");
          if (contactAllowed(account))
            actions.append(
              button("Refresh quota", () => refreshQuota([record.profileId])),
              button("Inspect credit status", () =>
                loadCredits(record.profileId),
              ),
            );
          appendResetReview(actions, record, {
            profileMissing: !account,
            readAt: Math.floor(state.lastRead / 1000),
          });
          section.append(actions);
          body.append(section);
        }
      },
    );
  }
  function showSettings() {
    const settings = state.inventory.settings || {};
    const inputs = {};
    const specs = [
      [
        "rotation_strategy",
        "Selection strategy",
        "select",
        "fill_first",
        "Priority order prefers lower priority numbers. Earliest reset prefers the eligible account whose quota resets first.",
        {
          options: [
            ["fill_first", t("Priority order")],
            ["earliest_reset", t("Earliest reset")],
          ],
        },
      ],
      [
        "preemptive_switch_percent",
        "Switch before used quota (%)",
        "number",
        95,
        "Switch before a hard limit. Use 0 or 100 to disable this threshold.",
        { min: 0, max: 100, step: "any", required: true },
      ],
      [
        "return_to_preferred",
        "Return to the preferred account",
        "checkbox",
        true,
        "Return to the lowest priority account when its cooldown expires.",
        {},
      ],
      [
        "window_warmup",
        "Warm up standby windows",
        "checkbox",
        true,
        "Makes small generating requests on standby accounts and uses a little quota. The active account stays selected.",
        {},
      ],
      [
        "window_warmup_interval_minutes",
        "Warmup interval (minutes)",
        "number",
        5,
        "The scheduler enforces a minimum of 5 minutes.",
        { min: 5, max: Number.MAX_SAFE_INTEGER, step: 1, required: true },
      ],
      [
        "resume_after_reset",
        "Resume interrupted work after reset",
        "checkbox",
        true,
        "Resume safely after quota recovery when the interrupted output can be reconciled.",
        {},
      ],
      [
        "max_reset_wait_minutes",
        "Maximum reset wait (minutes)",
        "number",
        360,
        "Maximum natural-reset wait for one interrupted turn, capped at 1440 minutes.",
        { min: 0, max: 1440, step: 1, required: true },
      ],
      [
        "auto_reset_credits",
        "Automatic reset credits",
        "select",
        "never",
        "When enabled, the scheduler may spend a credit only when the whole pool is exhausted and a natural reset is farther away than the minimum below.",
        {
          options: [
            ["never", t("Never (manual only)")],
            ["when_pool_exhausted", t("When the pool is exhausted")],
          ],
        },
      ],
      [
        "auto_reset_credit_min_wait_minutes",
        "Minimum wait before a credit (minutes)",
        "number",
        60,
        "Skip automatic redemption when a natural reset is within this many minutes. Waiting does not spend credits.",
        { min: 0, max: Number.MAX_SAFE_INTEGER, step: 1, required: true },
      ],
    ];
    openDialog(
      t("Edit pool settings"),
      "These settings are shared with clients using this Codex home. Active sessions apply them after configuration refresh.",
      (body) => {
        body.append(
          element(
            "p",
            t(
              "Warmup generates real requests. Automatic reset credits can spend a limited credit without another confirmation. Review these choices before saving.",
            ),
            "disclosure",
          ),
        );
        const preset = field(
          body,
          "preset",
          "Choose a behavior",
          "select",
          "current",
          "Presets never enable automatic reset credits or paid API fallback. Review the exact changes before saving.",
          {
            options: [
              ["current", t("Keep current settings")],
              ["earlier", t("Prefer earlier resets")],
              ["quiet", t("Reduce standby requests")],
            ],
          },
        );
        const preview = element("div", "", "disclosure");
        const grid = element("div", "", "settings-grid");
        function previewChanges() {
          const changes = specs.flatMap(([name, label, type, fallback]) => {
            const input = inputs[name];
            const value =
              type === "checkbox"
                ? input.checked
                : type === "number"
                  ? Number(input.value)
                  : input.value;
            if (value === (settings[name] ?? fallback)) return [];
            return [
              [label, type === "checkbox" ? t(value ? "On" : "Off") : value],
            ];
          });
          preview.replaceChildren(
            changes.length
              ? definitions(changes)
              : element("p", t("No changes selected.")),
          );
        }
        for (const [name, label, type, fallback, help, attributes] of specs) {
          let value = settings[name] ?? fallback;
          if (name === "window_warmup_interval_minutes")
            value = Math.max(5, value);
          if (name === "max_reset_wait_minutes") value = Math.min(1440, value);
          if (name === "auto_reset_credit_min_wait_minutes")
            value = Math.max(0, value);
          inputs[name] = field(
            grid,
            name,
            label,
            type,
            value,
            help,
            attributes,
          );
          inputs[name].addEventListener("change", previewChanges);
          inputs[name].addEventListener("input", previewChanges);
        }
        preset.addEventListener("change", () => {
          for (const [name, , type, fallback] of specs) {
            if (type === "checkbox")
              inputs[name].checked = settings[name] ?? fallback;
            else inputs[name].value = settings[name] ?? fallback;
          }
          if (preset.value === "earlier") {
            inputs.rotation_strategy.value = "earliest_reset";
            inputs.return_to_preferred.checked = false;
          } else if (preset.value === "quiet") {
            inputs.window_warmup.checked = false;
          }
          previewChanges();
        });
        previewChanges();
        body.append(preview, grid);
      },
      () => {
        const values = {};
        const review = [];
        for (const [name, label, type] of specs) {
          const input = inputs[name];
          values[name] =
            type === "checkbox"
              ? input.checked
              : type === "number"
                ? Number(input.value)
                : input.value;
          if (
            values[name] !==
            (settings[name] ?? specs.find((spec) => spec[0] === name)[3])
          )
            review.push([
              label,
              type === "checkbox"
                ? t(input.checked ? "On" : "Off")
                : values[name],
            ]);
          else delete values[name];
        }
        if (!review.length) {
          showResult("No changes selected.");
          return;
        }
        return perform({ type: "settings", values });
      },
      "Save pool settings",
    );
  }
  function apiValues(account) {
    return {
      id: account.id,
      label: account.label,
      baseUrl: account.baseUrl,
      model: account.model,
      disabled: account.disabled,
      contextWindow: account.contextWindow,
      images: account.images,
    };
  }
  function apiDetails(account) {
    return [
      [t("Profile ID"), account.id],
      [t("Provider endpoint"), account.baseUrl],
      [t("Model"), account.model],
      [t("Context limit"), account.contextWindow],
      [t("Images"), account.images ? t("Allowed") : t("Off")],
      [t("Local key"), account.hasKey ? t("Stored") : t("Missing")],
    ];
  }
  function apiCredentialRevision(account) {
    const revision = account?.credentialRevision;
    return typeof revision === "string" && /^[a-f0-9]{64}$/.test(revision)
      ? revision
      : null;
  }
  function renderApiAccounts() {
    const inventory = state.inventory;
    const accounts = inventory.apiAccounts || [];
    $("api-management").hidden = !Object.hasOwn(inventory, "apiAccounts");
    $("api-accounts").replaceChildren();
    const manual = inventory.apiSelection?.type === "manual";
    const current = accounts.find(
      (account) => account.id === inventory.apiSelection?.profileId,
    );
    $("api-billing").hidden = !manual;
    $("api-billing").textContent =
      current && !current.disabled && current.hasKey
        ? t(
            "Manual API target: {label}. Subsequent turns send conversation content to {url} and may incur provider charges. Select a subscription account or return to subscriptions to switch back.",
            { label: current.label, url: current.baseUrl },
          )
        : t(
            "The selected API profile is unavailable. Select a subscription account or another enabled API target.",
          );
    if (!accounts.length)
      $("api-accounts").append(
        element(
          "p",
          t(
            "No API accounts. Add one for explicit manual use; automatic paid fallback starts off.",
          ),
          "disclosure",
        ),
      );
    for (const account of accounts) {
      const card = element("article", "", "api-account");
      card.append(
        element("h3", account.label),
        element(
          "span",
          account.disabled
            ? t("Disabled")
            : account.hasKey
              ? t("Configured")
              : t("Key missing"),
          `badge ${account.disabled ? "disabled" : account.hasKey ? "ready" : "needsLogin"}`,
        ),
      );
      if (manual && current?.id === account.id)
        card.append(element("span", t("Selected API"), "badge active"));
      card.append(element("p", account.model, "muted"));
      const actions = element("div", "", "actions");
      const use = button("Use API account", () =>
        confirmOperation(
          "Use API account",
          "Subsequent turns send conversation content to this provider and are billed under its API account. This explicit selection is shared with clients using this Codex home.",
          {
            type: "apiUse",
            profileId: account.id,
            expectedCredentialRevision: apiCredentialRevision(account),
          },
          [[t("Account"), account.label], ...apiDetails(account)],
        ),
      );
      use.disabled =
        state.busy ||
        account.disabled ||
        !account.hasKey ||
        !apiCredentialRevision(account);
      if (!apiCredentialRevision(account))
        use.title = t(
          "The API account binding could not be verified. Reload account details before authorizing paid requests.",
        );
      actions.append(
        use,
        button("Edit API account", () => showApiEditor(account)),
        button(
          account.disabled ? "Enable API account" : "Disable API account",
          () =>
            confirmOperation(
              account.disabled ? "Enable API account" : "Disable API account",
              account.disabled
                ? "Make this provider target available for explicit selection or a configured paid fallback."
                : "Exclude this provider from subsequent selection and paid fallback. Already running requests are not cancelled.",
              {
                type: "apiUpdate",
                account: { ...apiValues(account), disabled: !account.disabled },
              },
              [
                [t("Account"), account.label],
                [t("Provider endpoint"), account.baseUrl],
              ],
            ),
        ),
        button(
          "Remove API account",
          () =>
            confirmOperation(
              "Remove API account",
              "Remove this provider profile and its local API key. A currently selected or configured fallback target will no longer be available.",
              { type: "apiRemove", profileId: account.id },
              [
                [t("Account"), account.label],
                [t("Provider endpoint"), account.baseUrl],
              ],
            ),
          "danger",
        ),
      );
      actions.insertBefore(
        button(account.hasKey ? "Replace key" : "Add missing key", () =>
          showApiKey(account),
        ),
        actions.children[2],
      );
      for (const action of actions.querySelectorAll("button"))
        action.dataset.focusKey = `${account.id}:${action.textContent}`;
      actions.removeChild(use);
      card.append(
        use,
        disclosure("Advanced controls", actions),
        disclosure("Technical details", definitions(apiDetails(account))),
      );
      $("api-accounts").append(card);
    }
    const fallback = inventory.apiFallback;
    const target = accounts.find(
      (account) => account.id === fallback?.profileId,
    );
    $("api-fallback-summary").textContent = fallback?.enabled
      ? target && !target.disabled && target.hasKey
        ? t(
            "Enabled: {label} after {minutes} minutes of subscription waiting. Provider charges may apply.",
            { label: target.label, minutes: fallback.waitMinutes },
          )
        : t(
            "Configured on, but the provider target is unavailable. Choose an enabled account with a stored key.",
          )
      : t(
          "Off. Subscription exhaustion will not automatically start paid API requests.",
        );
  }
  function showApiEditor(account = null) {
    const inputs = {};
    openDialog(
      account ? t("Edit API account") : t("Add API account"),
      "Use a provider endpoint and exact model ID that support the Responses API. Saving configuration sends no generating request and does not test compatibility.",
      (body) => {
        inputs.label = field(
          body,
          "apiLabel",
          "Label",
          "text",
          account?.label || "",
          "Choose a recognizable provider label.",
          { maxLength: 80, required: true },
        );
        inputs.baseUrl = field(
          body,
          "apiBaseUrl",
          "Provider base URL",
          "url",
          account?.baseUrl || "",
          "HTTPS, or HTTP on localhost. Include the API base path, such as /v1. Do not include /responses, credentials, a query, or a fragment.",
          { required: true, placeholder: "https://api.example.com/v1" },
        );
        inputs.model = field(
          body,
          "apiModel",
          "Model ID",
          "text",
          account?.model || "",
          "Use the exact ID advertised by your provider; capabilities are not inferred.",
          { maxLength: 256, required: true },
        );
        if (!account)
          inputs.apiKey = field(
            body,
            "apiKey",
            "API key",
            "password",
            "",
            "Saved only by the account manager in provider-specific credential storage. It will not be shown in results or kept in browser storage.",
            { required: true, autoComplete: "new-password", maxLength: 16384 },
          );
        const capabilities = element("div");
        inputs.contextWindow = field(
          capabilities,
          "apiContext",
          "Context limit (tokens)",
          "number",
          account?.contextWindow || 32768,
          "Use a limit supported by this model. The conservative default is 32768; allowed range is 8192–2000000.",
          { min: 8192, max: 2000000, step: 1, required: true },
        );
        inputs.images = field(
          capabilities,
          "apiImages",
          "Allow image inputs",
          "checkbox",
          account?.images || false,
          "Enable only when this provider and model accept images through the Responses API.",
        );
        const endpoint = element("p", "", "disclosure endpoint-preview");
        function previewEndpoint() {
          try {
            const url = new URL(inputs.baseUrl.value.trim());
            if (url.username || url.password || url.search || url.hash)
              throw new Error("Invalid base URL");
            url.pathname = url.pathname.replace(/\/+$/, "") + "/responses";
            endpoint.textContent = t("Requests use: {url}", { url: url.href });
          } catch {
            endpoint.textContent = t(
              "Enter a base URL to preview the Responses endpoint.",
            );
          }
        }
        inputs.baseUrl.addEventListener("input", previewEndpoint);
        previewEndpoint();
        body.append(
          endpoint,
          element(
            "p",
            t(
              "Native Responses API required. Chat Completions-only endpoints are not supported.",
            ),
            "muted",
          ),
          disclosure("Model capabilities", capabilities),
        );
      },
      () => {
        const values = {
          label: inputs.label.value.trim(),
          baseUrl: inputs.baseUrl.value.trim(),
          model: inputs.model.value.trim(),
          contextWindow: Number(inputs.contextWindow.value),
          images: inputs.images.checked,
        };
        let url;
        try {
          url = new URL(values.baseUrl);
        } catch {
          throw new Error(t("Enter a valid provider base URL."));
        }
        if (!values.label || !values.model)
          throw new Error(t("Enter an account label and exact model ID."));
        if (
          !(
            url.protocol === "https:" ||
            (url.protocol === "http:" &&
              ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))
          ) ||
          url.username ||
          url.password ||
          url.search ||
          url.hash
        )
          throw new Error(
            t(
              "Use an HTTPS endpoint without embedded credentials, query, or fragment. HTTP is allowed only on localhost.",
            ),
          );
        if (account) {
          confirmOperation(
            "Save API account",
            "Apply these endpoint and model settings. Subsequent use sends conversation content to this provider and may incur charges; saving sends no generating request.",
            {
              type: "apiUpdate",
              account: { ...apiValues(account), ...values },
            },
            [
              [t("Account"), values.label],
              [t("Provider endpoint"), values.baseUrl],
              [t("Model"), values.model],
              [t("Context limit"), values.contextWindow],
              [t("Images"), values.images ? t("Allowed") : t("Off")],
            ],
          );
          return;
        }
        const payload = {
          type: "apiAdd",
          ...values,
          apiKey: inputs.apiKey.value.trim(),
        };
        inputs.apiKey.value = "";
        if (!payload.apiKey)
          throw new Error(t("Enter an API key before saving."));
        openDialog(
          t("Save API account"),
          "Save this provider and its key for explicit manual use. No generating request is sent. Usage after selection is billed by the provider.",
          (body) =>
            body.append(
              definitions([
                ["Account", values.label],
                ["Provider endpoint", values.baseUrl],
                ["Model", values.model],
                ["Context limit", values.contextWindow],
                ["Images", values.images ? t("Allowed") : t("Off")],
                ["API key", t("Entered; never displayed")],
              ]),
            ),
          async () => {
            try {
              await perform(payload);
            } finally {
              payload.apiKey = "";
              dialogCompleted = true;
              $("dialog-submit").disabled = true;
              $("dialog-cancel").textContent = t("Close");
            }
          },
          "Save API account",
        );
      },
      "Review API account",
    );
  }
  function showApiFallback() {
    const fallback = state.inventory.apiFallback || {
      enabled: false,
      profileId: null,
      waitMinutes: 5,
    };
    let enabled, target, wait;
    openDialog(
      t("Configure paid API fallback"),
      "Off by default. Enabling this can send conversation content to a provider and incur charges after subscription waiting, without another per-turn confirmation.",
      (body) => {
        enabled = field(
          body,
          "fallbackEnabled",
          "Allow automatic paid API fallback",
          "checkbox",
          fallback.enabled,
        );
        const options = [
          ["", t("Choose a provider target")],
          ...(state.inventory.apiAccounts || [])
            .filter(
              (account) =>
                !account.disabled &&
                account.hasKey &&
                apiCredentialRevision(account),
            )
            .map((account) => [
              account.id,
              `${account.label} (${account.model})`,
            ]),
        ];
        target = field(
          body,
          "fallbackTarget",
          "Provider target",
          "select",
          fallback.profileId || "",
          "Only enabled profiles with a stored key and verified account details are eligible.",
          { options },
        );
        wait = field(
          body,
          "fallbackWait",
          "Subscription waiting time (minutes)",
          "number",
          fallback.waitMinutes,
          "0 allows immediate paid fallback after subscription exhaustion. Wait up to 1440 minutes to allow natural quota recovery first.",
          { min: 0, max: 1440, step: 1, required: true },
        );
      },
      () => {
        const account = state.inventory.apiAccounts.find(
          (account) => account.id === target.value,
        );
        if (
          enabled.checked &&
          (!account ||
            account.disabled ||
            !account.hasKey ||
            !apiCredentialRevision(account))
        )
          throw new Error(
            t(
              "Select an enabled API account with a stored key before enabling paid fallback.",
            ),
          );
        const config = {
          enabled: enabled.checked,
          profileId: target.value || null,
          waitMinutes: Number(wait.value),
        };
        confirmOperation(
          "Save API fallback",
          config.enabled
            ? "Authorize automatic paid requests to this exact provider after the waiting time shown. Conversation content is sent to this provider and its API billing applies."
            : "Disable automatic paid fallback. Explicit manual API selection remains available.",
          {
            type: "apiFallback",
            config,
            expectedCredentialRevision: config.enabled
              ? apiCredentialRevision(account)
              : null,
          },
          [
            [
              t("Automatic paid fallback"),
              config.enabled ? t("Enabled") : t("Off"),
            ],
            [t("Provider"), account?.label || t("None")],
            [t("Provider endpoint"), account?.baseUrl || t("None")],
            [t("Model"), account?.model || t("None")],
            [
              t("Subscription wait"),
              t("{minutes} minutes", { minutes: config.waitMinutes }),
            ],
          ],
        );
      },
      "Review fallback",
    );
  }
  function showApiKey(account) {
    let key;
    openDialog(
      account.hasKey ? t("Replace API key") : t("Add API key"),
      "Change only the local credential for this exact profile. Its endpoint, model, selection, and fallback settings are preserved. No generating request is sent.",
      (body) => {
        body.append(
          definitions([
            ["Account", account.label],
            ["Profile ID", account.id],
            ["Provider endpoint", account.baseUrl],
          ]),
        );
        key = field(
          body,
          "replacementKey",
          "New API key",
          "password",
          "",
          "The old credential is replaced only after you confirm. The new key will not be displayed or kept in browser storage.",
          { required: true, autoComplete: "new-password", maxLength: 16384 },
        );
      },
      () => {
        const payload = {
          type: "apiReplaceKey",
          profileId: account.id,
          apiKey: key.value.trim(),
        };
        key.value = "";
        if (!payload.apiKey)
          throw new Error(t("Enter a new API key before continuing."));
        openDialog(
          t("Confirm API key replacement"),
          "Replace the stored API key for the exact profile shown below. No request is sent to this provider to test the key.",
          (body) =>
            body.append(
              definitions([
                ["Account", account.label],
                ["Profile ID", account.id],
                ["Provider endpoint", account.baseUrl],
                ["New API key", t("Entered; never displayed")],
              ]),
            ),
          async () => {
            try {
              await perform(payload);
            } finally {
              payload.apiKey = "";
              dialogCompleted = true;
              $("dialog-submit").disabled = true;
              $("dialog-cancel").textContent = t("Close");
            }
          },
          "Replace stored key",
        );
      },
      "Review key replacement",
    );
  }
  $("action-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (
      state.busy ||
      dialogCompleted ||
      !dialogTask ||
      !$("action-form").reportValidity()
    )
      return;
    try {
      await dialogTask();
    } catch (error) {
      showResult(error.message, true);
      notice(error.message, true);
    }
  });
  $("dialog-cancel").addEventListener("click", () => {
    if (!state.busy) $("action-dialog").close();
  });
  $("action-dialog").addEventListener("cancel", (event) => {
    if (state.busy) event.preventDefault();
  });
  $("action-dialog").addEventListener("close", () => {
    dialogTask = null;
    $("language").disabled = state.busy || state.preferenceSaving;
    const replacement =
      returnFocusKey &&
      [...$("app").querySelectorAll("[data-focus-key]")].find(
        (node) => node.dataset.focusKey === returnFocusKey,
      );
    if (!state.paired) $("pair-token").focus();
    else if (returnFocus?.isConnected) returnFocus.focus();
    else if (replacement) replacement.focus();
    else $("add-account").focus();
  });
  $("read-status").addEventListener("click", () => {
    readInventory().catch(() => {});
  });
  $("stop-manager").addEventListener("click", () => {
    openDialog(
      t("Stop account manager?"),
      "This stops the account manager for every browser tab and cancels pending logins. Work in Codex sessions continues. Existing account changes finish before the process exits.",
      (body) =>
        body.append(
          element(
            "p",
            t("Start codex account manage again to reopen the panel."),
          ),
        ),
      async () => {
        setBusy(true);
        try {
          await request("/api/shutdown", {});
          window.AccountManagerLifecycle.stop();
          clearTimeout(state.timer);
          setPaired(false);
          state.stopped = true;
          $("action-dialog").close();
          $("pair-panel").hidden = true;
          $("read-status").disabled = true;
          $("connection-state").textContent = t("Manager stopped");
          notice("Account manager stopped. You can close this tab.");
        } finally {
          setBusy(false);
        }
      },
      "Stop manager",
    );
  });
  $("add-account").addEventListener("click", () => showLogin());
  $("edit-settings").addEventListener("click", showSettings);
  $("add-api-account").addEventListener("click", () => showApiEditor());
  $("edit-api-fallback").addEventListener("click", showApiFallback);
  $("refresh-all").addEventListener("click", () => refreshQuota());
  $("review-reset").addEventListener("click", showPendingReset);
  $("automatic").addEventListener("click", () =>
    confirmOperation(
      "Return to subscriptions",
      "Exit manual API selection and use subscription scheduling for subsequent requests. If no account is eligible, existing cooldowns are kept. Your configured paid fallback policy still applies.",
      { type: "automatic" },
    ),
  );
  for (const id of ["account-search", "account-filter"])
    $(id).addEventListener(id === "account-search" ? "input" : "change", () => {
      state.page = 0;
      if (state.inventory) renderAccounts();
    });
  $("previous-page").addEventListener("click", () => {
    state.page--;
    renderAccounts();
  });
  $("next-page").addEventListener("click", () => {
    state.page++;
    renderAccounts();
  });
  document.addEventListener("visibilitychange", () => {
    clearTimeout(state.timer);
    $("poll-note").textContent = document.hidden
      ? t("Metadata updates are paused while this page is hidden.")
      : t(
          "Metadata reads do not refresh backend quota. Updates are faster during login or quota checks, and slower while idle.",
        );
    if (!document.hidden && state.paired) readInventory().catch(() => {});
  });
  $("language").value = messages.language();
  async function saveLanguage() {
    if (!state.paired || state.stopped || state.preferenceSaving) return;
    state.preferenceSaving = true;
    $("language").disabled = true;
    try {
      await request("/api/preferences", { language: messages.language() });
      state.languageDirty = false;
      $("language-notice").textContent = t(
        "Language saved for browser and terminal managers.",
      );
      $("language-notice").hidden = false;
    } catch (error) {
      $("language-notice").textContent = t(
        "Language changed for this tab; saving failed: {message}",
        { message: error.message },
      );
      $("language-notice").hidden = false;
      // Retry only after a fresh user selection, rather than on every metadata poll.
      state.languageDirty = false;
    } finally {
      state.preferenceSaving = false;
      $("language").disabled =
        state.stopped || state.busy || $("action-dialog").open;
    }
  }
  $("language").addEventListener("change", () => {
    if ($("action-dialog").open || state.busy) return;
    messages.setLanguage($("language").value);
    state.languageDirty = true;
    if (state.stopped) {
      $("connection-state").textContent = t("Manager stopped");
      notice("Account manager stopped. You can close this tab.");
      return;
    }
    setPaired(state.paired);
    if (state.inventory) renderInventory();
    notice(state.paired ? "Account status is ready." : "Pairing required");
    saveLanguage().catch(() => {});
  });
  async function pair(token) {
    history.replaceState(null, "", location.pathname + location.search);
    $("pair-token").value = "";
    setPaired(false);
    try {
      const result = await request("/api/session", { token });
      if (
        result.paired !== true ||
        typeof result.sessionToken !== "string" ||
        !result.sessionToken ||
        result.sessionToken.length > 256
      )
        throw new Error(
          t(
            "The manager returned an invalid pairing session. Pair this browser again.",
          ),
        );
      state.sessionToken = result.sessionToken;
      try {
        sessionStorage.setItem(sessionStorageKey, state.sessionToken);
      } catch {
        notice(
          "Session storage is unavailable. This browser is paired until the page is reloaded.",
        );
      }
      await readInventory();
      notice("Browser paired. Account status is ready.");
    } catch (error) {
      setPaired(false);
      $("global-error").textContent = error.message;
      $("global-error").hidden = false;
    } finally {
      $("pair-token").value = "";
      history.replaceState(null, "", location.pathname + location.search);
    }
  }
  $("pair-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const token = $("pair-token").value.trim();
    if (!token) return;
    const submit = $("pair-form").querySelector("button");
    submit.disabled = true;
    try {
      await pair(token);
    } finally {
      submit.disabled = false;
    }
  });
  try {
    const sessionToken = sessionStorage.getItem(sessionStorageKey);
    if (
      typeof sessionToken === "string" &&
      sessionToken.length > 0 &&
      sessionToken.length <= 256
    )
      state.sessionToken = sessionToken;
    else sessionStorage.removeItem(sessionStorageKey);
  } catch {
    /* Pairing can stay in memory when session storage is unavailable. */
  }
  window.AccountManagerUI = Object.freeze({
    element,
    button,
    definitions,
    request,
    operation,
    readInventory,
    openDialog,
    confirmOperation,
    notice,
  });
  const token = new URLSearchParams(location.hash.slice(1)).get("pair");
  if (token) pair(token);
  else
    readInventory()
      .then(() => notice("Account status is ready."))
      .catch(() => {});
})();
