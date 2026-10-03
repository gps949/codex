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
    redemption: null,
    sessionToken: "",
  };
  const resetStorageKey = "codex.accountManager.resetOperation";
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
    if (text !== "") node.textContent = String(text);
    if (className) node.className = className;
    return node;
  }
  function button(text, action, className = "") {
    const node = element("button", t(text), className);
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
    if (known)
      node.append(
        element(
          "small",
          t("Remaining {percent}% (cached)", {
            percent: Number(
              Math.max(0, Math.min(100, 100 - window.usedPercent)).toFixed(1),
            ),
          }),
        ),
      );
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
      throw new Error("Account manager requests must use this browser origin.");
    const headers =
      payload === undefined ? {} : { "Content-Type": "application/json" };
    if (url.pathname !== "/api/session") {
      if (!state.sessionToken) {
        setPaired(false);
        const error = new Error(
          "Pair this browser with the account manager first.",
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
          ? "The connection timed out. The operation may still be running; check its status before repeating it."
          : "Connection interrupted. Check the account manager and read status before repeating an operation.",
      );
    }
    let data;
    try {
      data = await response.json();
    } catch {
      if (response.ok)
        throw new Error(
          "The account manager returned an unreadable result. Check status before repeating the operation.",
        );
      data = {};
    }
    if (!response.ok) {
      const secretOperation =
        payload?.type === "apiAdd" || payload?.type === "apiReplaceKey";
      const error = new Error(
        secretOperation
          ? `The API credential operation failed (HTTP ${response.status}). Check account status and the provider settings. Re-enter the key to try again.`
          : url.pathname === "/api/session"
            ? "Pairing was denied. Open the current pairing URL printed by the manager."
            : data.error ||
              `The account manager returned HTTP ${response.status}.`,
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
        "The operation returned an incomplete result. Check status before repeating it.",
      );
    if (payload.type === "apiAdd")
      result.message =
        "API account saved for manual selection. No generating request was sent.";
    if (payload.type === "apiReplaceKey")
      result.message =
        "API key replaced for this profile. No generating request was sent.";
    return result;
  }
  function setPaired(paired) {
    state.paired = paired;
    $("pair-panel").hidden = paired;
    $("dashboard").hidden = !paired;
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
      if ($("action-dialog").open) $("action-dialog").close();
    }
  }
  function scheduleRead() {
    clearTimeout(state.timer);
    if (state.paired && !document.hidden)
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
    if (state.reading) return state.reading;
    state.reading = (async () => {
      try {
        const inventory = await request("/api/inventory");
        if (!Array.isArray(inventory.accounts))
          throw new Error(
            t("The account manager returned an invalid inventory."),
          );
        guidance.setClock(inventory.hostNow);
        const inventoryKey = JSON.stringify({ ...inventory, hostNow: null });
        const changed = inventoryKey !== state.inventoryKey;
        state.inventory = inventory;
        state.inventoryKey = inventoryKey;
        state.lastRead = Date.now();
        state.readFailed = false;
        setPaired(true);
        $("global-error").hidden = true;
        if (changed) renderInventory();
        else updateClocks();
        window.dispatchEvent(
          new CustomEvent("accountmanager:inventory", { detail: inventory }),
        );
        return inventory;
      } catch (error) {
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
        ? `${guidance.status(selected.availability)}. ${guidance.reason(selected)}`
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
    $("automatic").disabled = state.busy;
    $("refresh-all").disabled =
      state.busy ||
      !inventory.accounts.some(
        (account) => contactAllowed(account) && !account.refresh?.inProgress,
      );
    $("pending-reset").hidden = !state.redemption;
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
      identity.append(
        element(
          "p",
          [account.plan, account.email].filter(Boolean).join(" · ") ||
            t("Subscription account"),
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
      buttons.append(primaryAction, manage);
      if (account.availability !== "ready")
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
    $("dialog-submit").disabled = busy || dialogCompleted;
    $("action-form").setAttribute("aria-busy", String(busy));
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
    $("dialog-result").textContent = message;
    $("dialog-result").className = `message ${failed ? "error" : "success"}`;
    $("dialog-result").hidden = false;
    $("dialog-cancel").textContent = dialogCompleted ? "Close" : "Cancel";
  }
  function openDialog(
    title,
    description,
    build,
    submit = null,
    submitLabel = "Confirm",
  ) {
    if (state.busy) return;
    if (!$("action-dialog").open) {
      returnFocus = document.activeElement;
      returnFocusKey = returnFocus?.dataset.focusKey;
    }
    dialogTask = submit;
    dialogCompleted = false;
    $("dialog-title").textContent = title;
    $("dialog-description").textContent = description;
    $("dialog-body").replaceChildren();
    $("dialog-result").hidden = true;
    $("dialog-submit").hidden = !submit;
    $("dialog-submit").disabled = false;
    $("dialog-submit").textContent = submitLabel;
    $("dialog-cancel").textContent = submit ? "Cancel" : "Close";
    build($("dialog-body"));
    if (!$("action-dialog").open) $("action-dialog").showModal();
    $("dialog-title").focus();
  }
  async function perform(payload) {
    if (state.busy)
      throw new Error("Wait for the current operation to finish.");
    setBusy(true);
    try {
      const result = await operation(payload);
      dialogCompleted = true;
      if ($("action-dialog").open)
        showResult(result.message || "Operation completed.");
      notice(result.message || "Operation completed.");
      await readInventory().catch(() => {});
      return result;
    } finally {
      setBusy(false);
    }
  }
  function confirmOperation(title, description, payload, items = []) {
    openDialog(
      title,
      description,
      (body) => body.append(definitions(items)),
      () => perform(payload),
      title,
    );
  }
  async function refreshQuota(ids = null) {
    try {
      notice("Checking backend quota…");
      await perform({ type: "refresh", profileIds: ids });
    } catch (error) {
      notice(`Quota refresh failed: ${error.message}`, true);
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
            ["Profile ID", id],
            ["Email", account.email],
            ["Plan", account.plan],
            ["Login", statusNames[account.loginState] || account.loginState],
            [
              "Availability",
              statusNames[account.availability] || account.availability,
            ],
            ["Priority", account.priority],
            ["Reset credits", account.resetCreditCount],
            ["Backend reset", date(account.backendResetsAt)],
          ]),
        );
        const quotas = element("div", "", "detail-quotas");
        quotas.append(quota(account, "primary"), quota(account, "secondary"));
        body.append(quotas);
        if (account.refresh)
          body.append(
            element(
              "p",
              `Last refresh ${date(account.refresh.attemptedAt)}: ${account.refresh.message}`,
              "disclosure",
            ),
          );
        const actions = element("div", "", "actions detail-actions");
        const use = button(
          "Use account",
          () =>
            confirmOperation(
              "Use account",
              "Use this account for subsequent requests. Requests already running keep their current identity.",
              { type: "use", profileId: id },
              [
                ["Account", account.label],
                ["Profile ID", id],
              ],
            ),
          "primary",
        );
        use.disabled =
          !contactAllowed(account) || account.availability === "coolingDown";
        const retry = button("Retry without credit", () =>
          confirmOperation(
            "Retry without credit",
            "Clear this account's local quota cooldown for one real request. This selects the account; the backend may still reject it. No reset credit is consumed.",
            { type: "retry", profileId: id },
            [
              ["Account", account.label],
              ["Profile ID", id],
            ],
          ),
        );
        retry.disabled = !contactAllowed(account);
        const credits = button("Use reset credit", () => loadCredits(id));
        credits.disabled = !contactAllowed(account);
        const refresh = button("Refresh quota", () => refreshQuota([id]));
        refresh.disabled = !contactAllowed(account);
        actions.append(
          use,
          retry,
          refresh,
          credits,
          button(
            account.loginState === "signedIn"
              ? "Sign in again"
              : "Complete login",
            () => showLogin(id),
          ),
          button("Edit label & priority", () => showEdit(id)),
          button(account.disabled ? "Enable account" : "Disable account", () =>
            confirmOperation(
              account.disabled ? "Enable account" : "Disable account",
              account.disabled
                ? "Allow this account to participate in the pool again. Login and quota still determine eligibility."
                : "Exclude this account from future pool selection. This does not cancel a request already in progress.",
              { type: "update", profileId: id, disabled: !account.disabled },
              [
                ["Account", account.label],
                ["Profile ID", id],
              ],
            ),
          ),
          button("Remove account", () => showRemove(id), "danger"),
        );
        body.append(actions);
      },
    );
  }
  function showEdit(id) {
    const account = accountById(id);
    let label, priority;
    openDialog(
      "Edit account",
      "A lower priority number is preferred. Clearing the label displays the profile ID.",
      (body) => {
        body.append(definitions([["Profile ID", id]]));
        label = field(
          body,
          "label",
          "Label",
          "text",
          account.label === id ? "" : account.label,
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
      () =>
        perform({
          type: "update",
          profileId: id,
          label: label.value.trim(),
          priority: Number(priority.value),
        }),
      "Save account",
    );
  }
  function showRemove(id) {
    const account = accountById(id);
    let keep;
    openDialog(
      "Remove account",
      "Remove this profile from the pool. It will no longer be available for selection or automatic failover.",
      (body) => {
        body.append(
          definitions([
            ["Account", account.label],
            ["Profile ID", id],
          ]),
        );
        keep = field(
          body,
          "keepCredentials",
          "Keep local sign-in credentials",
          "checkbox",
          true,
          id === "legacy-root"
            ? "Root sign-in credentials are always retained. Only this pool entry is removed."
            : "Checked: stored credentials stay on this computer. Unchecked: local credentials are deleted and server revocation is attempted; the server may retain a session.",
        );
        if (id === "legacy-root") keep.disabled = true;
      },
      () =>
        perform({
          type: "remove",
          profileId: id,
          keepCredentials: keep.checked,
        }),
      "Remove account",
    );
  }
  function showLogin(id = null) {
    let label;
    openDialog(
      id ? "Sign in again" : "Add account",
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
        label: "Credit identity or scope could not be verified",
      };
    if (credit.status !== "available")
      return {
        eligible: false,
        label: `Unavailable (${credit.status || "unknown status"})`,
      };
    if (credit.expiresAt === null || credit.expiresAt === undefined)
      return { eligible: true, label: "Available; no expiry reported" };
    const expiry = Date.parse(credit.expiresAt);
    if (!Number.isFinite(expiry))
      return { eligible: false, label: "Expiry could not be verified" };
    return expiry > Date.now()
      ? { eligible: true, label: "Available; not expired" }
      : { eligible: false, label: "Expired" };
  }
  function creditDetails(credit) {
    return [
      ["Credit ID", credit.id],
      ["Backend status", credit.status],
      ["Validity", creditValidity(credit).label],
      [
        "Expires",
        credit.expiresAt
          ? `${date(credit.expiresAt)} (${credit.expiresAt})`
          : "Not reported",
      ],
      ["Scope", credit.resetType],
      ["Granted", date(credit.grantedAt)],
    ];
  }
  async function loadCredits(id) {
    openDialog(
      "Reset credits",
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
        throw new Error("Credit details did not match the selected profile.");
      const readAt = Math.floor(Date.now() / 1000);
      await readInventory();
      setBusy(false);
      showCredits(id, result.data, readAt);
      notice(result.message || "Reset credits loaded.");
    } catch (error) {
      setBusy(false);
      showResult(error.message, true);
    }
  }
  function showCredits(id, data, readAt) {
    let selected = null;
    const account = accountById(id);
    const pending = state.redemption;
    openDialog(
      "Choose a reset credit",
      "Reading credits does not consume them. Choose one available credit, then review a separate confirmation.",
      (body) => {
        body.append(
          definitions([
            ["Account", account?.label],
            ["Profile ID", id],
            ["Available credits", data.availableCount],
          ]),
        );
        if (!contactAllowed(account))
          body.append(
            element(
              "p",
              "Enable this account and complete login before consuming a credit.",
              "message warning",
            ),
          );
        if (pending)
          body.append(
            element(
              "p",
              "An earlier operation is unconfirmed. You may retry its same credit with the same operation ID after checking the details below.",
              "disclosure",
            ),
          );
        if (!data.credits.length)
          body.append(
            element(
              "p",
              "No reset credits were returned for this account.",
              "disclosure",
            ),
          );
        for (const credit of data.credits) {
          const choice = element("label", "", "credit-choice");
          const input = element("input");
          input.type = "radio";
          input.name = "credit";
          input.value = credit.id;
          input.required = true;
          input.disabled =
            !contactAllowed(account) ||
            !creditValidity(credit).eligible ||
            (pending &&
              (pending.profileId !== id || pending.creditId !== credit.id));
          input.addEventListener("change", () => {
            selected = credit;
          });
          const content = element("div");
          content.append(element("strong", credit.title || "Reset credit"));
          if (credit.description)
            content.append(element("p", credit.description));
          content.append(definitions(creditDetails(credit)));
          choice.append(input, content);
          body.append(choice);
        }
        const prior = data.credits.find(
          (credit) => credit.id === pending?.creditId,
        );
        if (
          pending?.profileId === id &&
          (!prior || !creditValidity(prior).eligible)
        )
          body.append(
            button("Clear reviewed operation", () =>
              confirmOperationClear(pending, { credit: prior, readAt }),
            ),
          );
      },
      () => {
        if (!selected)
          throw new Error("Select an available credit before continuing.");
        showRedemption(id, selected);
      },
      "Review selected credit",
    );
  }
  function saveRedemption(value) {
    try {
      if (value) sessionStorage.setItem(resetStorageKey, JSON.stringify(value));
      else sessionStorage.removeItem(resetStorageKey);
    } catch {
      throw new Error(
        "Browser session storage is unavailable. Enable it before consuming a credit so an uncertain operation can be retried safely.",
      );
    }
    state.redemption = value;
    $("pending-reset").hidden = !value;
  }
  function showRedemption(id, credit) {
    let acknowledged;
    openDialog(
      "Use this reset credit",
      "This consumes a limited credit for the exact profile shown below. Its backend scope is shown without assuming which quota windows it resets.",
      (body) => {
        body.append(
          definitions([
            ["Account", accountById(id)?.label],
            ["Profile ID", id],
            ...creditDetails(credit),
          ]),
        );
        if (state.redemption)
          body.append(
            definitions([
              ["Existing operation ID", state.redemption.idempotencyKey],
            ]),
          );
        acknowledged = field(
          body,
          "acknowledge",
          "I confirm the account, credit, and scope. Consume this credit.",
          "checkbox",
          false,
          "An uncertain result keeps the same operation ID; this page never repeats redemption automatically.",
          { required: true },
        );
      },
      async () => {
        if (
          !acknowledged.checked ||
          !creditValidity(credit).eligible ||
          !contactAllowed(accountById(id))
        )
          throw new Error(
            "This credit or account is no longer eligible. Read the current details first.",
          );
        let pending = state.redemption;
        if (
          pending &&
          (pending.profileId !== id || pending.creditId !== credit.id)
        )
          throw new Error(
            "Review the earlier unconfirmed operation before starting another reset.",
          );
        if (!pending) {
          if (!crypto.randomUUID)
            throw new Error(
              "Open this manager through HTTPS or localhost before consuming a credit.",
            );
          pending = {
            profileId: id,
            creditId: credit.id,
            idempotencyKey: crypto.randomUUID(),
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
          });
          saveRedemption(null);
        } catch (error) {
          throw new Error(
            `${error.message} The outcome remains unconfirmed. Refresh quota and inspect this credit before explicitly retrying the same operation.`,
          );
        }
      },
      state.redemption
        ? "Retry same credit operation"
        : "Consume selected credit",
    );
  }
  function confirmOperationClear(pending, evidence) {
    let acknowledged;
    const credit = evidence.credit;
    openDialog(
      "Clear reviewed reset operation",
      evidence.profileMissing
        ? "A fresh account read shows the original profile is no longer enrolled. Its redemption outcome cannot be checked here. Clear only this browser's retry record after reviewing the uncertainty; no credit will be consumed."
        : "The latest successful credit read shows this credit is absent or unavailable. This does not prove whether the earlier operation consumed it. Clear only this browser's retry record; no credit will be consumed.",
      (body) => {
        body.append(
          definitions([
            ["Profile ID", pending.profileId],
            ["Credit ID", pending.creditId],
            [
              "Review evidence",
              evidence.profileMissing
                ? "Profile absent from current inventory"
                : credit
                  ? `Credit unavailable (${credit.status})`
                  : "Credit absent from latest returned list",
            ],
            [
              "Validity",
              credit ? creditValidity(credit).label : "Outcome unconfirmed",
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
          "Clearing allows a different manual credit operation. The old operation will not be retried or consumed by this action.",
          { required: true },
        );
      },
      () => {
        if (!acknowledged.checked)
          throw new Error(
            "Acknowledge the unconfirmed outcome before clearing this record.",
          );
        if (state.redemption?.idempotencyKey !== pending.idempotencyKey)
          throw new Error(
            "The pending reset operation changed. Review its current record again.",
          );
        if (evidence.profileMissing && accountById(pending.profileId))
          throw new Error(
            "This profile has reappeared. Inspect its current credits before clearing the record.",
          );
        saveRedemption(null);
        dialogCompleted = true;
        showResult("Reviewed operation record cleared.");
        $("dialog-submit").disabled = true;
      },
      "Clear reviewed operation",
    );
  }
  async function showPendingReset() {
    const pending = state.redemption;
    if (!pending) return;
    openDialog(
      "Unconfirmed reset operation",
      "Reading current account status before reviewing this operation…",
      (body) =>
        body.append(
          definitions([
            ["Profile ID", pending.profileId],
            ["Credit ID", pending.creditId],
            ["Operation ID", pending.idempotencyKey],
          ]),
        ),
    );
    setBusy(true);
    try {
      await readInventory();
    } catch (error) {
      setBusy(false);
      showResult(error.message, true);
      return;
    }
    setBusy(false);
    const account = accountById(pending.profileId);
    const readAt = Math.floor(state.lastRead / 1000);
    openDialog(
      "Unconfirmed reset operation",
      "It may have reached the backend. Refresh quota and inspect the credit status before retrying. A retry keeps this operation ID.",
      (body) => {
        body.append(
          definitions([
            ["Account", account?.label || "Profile no longer enrolled"],
            ["Profile ID", pending.profileId],
            ["Credit ID", pending.creditId],
            ["Operation ID", pending.idempotencyKey],
            ["Started", date(pending.startedAt)],
          ]),
        );
        const actions = element("div", "", "actions");
        if (account)
          actions.append(
            button("Refresh quota", () => refreshQuota([pending.profileId])),
            button("Inspect credit status", () =>
              loadCredits(pending.profileId),
            ),
          );
        else
          actions.append(
            button("Clear reviewed operation", () =>
              confirmOperationClear(pending, { profileMissing: true, readAt }),
            ),
          );
        body.append(actions);
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
            ["fill_first", "Priority order"],
            ["earliest_reset", "Earliest reset"],
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
            ["never", "Never (manual only)"],
            ["when_pool_exhausted", "When the pool is exhausted"],
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
      "Edit pool settings",
      "These settings are shared with clients using this Codex home. Active sessions apply them after configuration refresh.",
      (body) => {
        body.append(
          element(
            "p",
            "Warmup generates real requests. Automatic reset credits can spend a limited credit without another confirmation. Review these choices before saving.",
            "disclosure",
          ),
        );
        const grid = element("div", "", "settings-grid");
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
        }
        body.append(grid);
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
          review.push([
            label,
            type === "checkbox" ? (input.checked ? "On" : "Off") : values[name],
          ]);
        }
        confirmOperation(
          "Save pool settings",
          "Apply these choices to this shared account pool. Automatic reset credits may spend a credit when enabled; standby warmup uses small generating requests.",
          { type: "settings", values },
          review,
        );
      },
      "Review settings",
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
      ["Profile ID", account.id],
      ["Provider endpoint", account.baseUrl],
      ["Model", account.model],
      ["Context limit", account.contextWindow],
      ["Images", account.images ? "Allowed" : "Off"],
      ["Local key", account.hasKey ? "Stored" : "Missing"],
    ];
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
        ? `Manual API target: ${current.label}. Subsequent turns send conversation content to ${current.baseUrl} and may incur provider charges. Select a subscription account or return to subscriptions to switch back.`
        : "The selected API profile is unavailable. Select a subscription account or another enabled API target.";
    if (!accounts.length)
      $("api-accounts").append(
        element(
          "p",
          "No API accounts. Add one for explicit manual use; automatic paid fallback starts off.",
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
            ? "Disabled"
            : account.hasKey
              ? "Configured"
              : "Key missing",
          `badge ${account.disabled ? "disabled" : account.hasKey ? "ready" : "needsLogin"}`,
        ),
      );
      if (manual && current?.id === account.id)
        card.append(element("span", "Selected API", "badge active"));
      card.append(definitions(apiDetails(account)));
      const actions = element("div", "", "actions");
      const use = button("Use API account", () =>
        confirmOperation(
          "Use API account",
          "Subsequent turns send conversation content to this provider and are billed under its API account. This explicit selection is shared with clients using this Codex home.",
          { type: "apiUse", profileId: account.id },
          [["Account", account.label], ...apiDetails(account)],
        ),
      );
      use.disabled = state.busy || account.disabled || !account.hasKey;
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
                ["Account", account.label],
                ["Provider endpoint", account.baseUrl],
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
                ["Account", account.label],
                ["Provider endpoint", account.baseUrl],
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
      card.append(actions);
      $("api-accounts").append(card);
    }
    const fallback = inventory.apiFallback;
    const target = accounts.find(
      (account) => account.id === fallback?.profileId,
    );
    $("api-fallback-summary").textContent = fallback?.enabled
      ? target && !target.disabled && target.hasKey
        ? `Enabled: ${target.label} after ${fallback.waitMinutes} minutes of subscription waiting. Provider charges may apply.`
        : "Configured on, but the provider target is unavailable. Choose an enabled account with a stored key."
      : "Off. Subscription exhaustion will not automatically start paid API requests.";
  }
  function showApiEditor(account = null) {
    const inputs = {};
    openDialog(
      account ? "Edit API account" : "Add API account",
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
        inputs.contextWindow = field(
          body,
          "apiContext",
          "Context limit (tokens)",
          "number",
          account?.contextWindow || 32768,
          "Use a limit supported by this model. The conservative default is 32768; allowed range is 8192–2000000.",
          { min: 8192, max: 2000000, step: 1, required: true },
        );
        inputs.images = field(
          body,
          "apiImages",
          "Allow image inputs",
          "checkbox",
          account?.images || false,
          "Enable only when this provider and model accept images through the Responses API.",
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
          throw new Error("Enter a valid provider base URL.");
        }
        if (!values.label || !values.model)
          throw new Error("Enter an account label and exact model ID.");
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
            "Use an HTTPS endpoint without embedded credentials, query, or fragment. HTTP is allowed only on localhost.",
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
              ["Account", values.label],
              ["Provider endpoint", values.baseUrl],
              ["Model", values.model],
              ["Context limit", values.contextWindow],
              ["Images", values.images ? "Allowed" : "Off"],
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
        if (!payload.apiKey) throw new Error("Enter an API key before saving.");
        openDialog(
          "Save API account",
          "Save this provider and its key for explicit manual use. No generating request is sent. Usage after selection is billed by the provider.",
          (body) =>
            body.append(
              definitions([
                ["Account", values.label],
                ["Provider endpoint", values.baseUrl],
                ["Model", values.model],
                ["Context limit", values.contextWindow],
                ["Images", values.images ? "Allowed" : "Off"],
                ["API key", "Entered; never displayed"],
              ]),
            ),
          async () => {
            try {
              await perform(payload);
            } finally {
              payload.apiKey = "";
              dialogCompleted = true;
              $("dialog-submit").disabled = true;
              $("dialog-cancel").textContent = "Close";
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
      "Configure paid API fallback",
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
          ["", "Choose a provider target"],
          ...(state.inventory.apiAccounts || [])
            .filter((account) => !account.disabled && account.hasKey)
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
          "Only enabled profiles with a stored key are eligible.",
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
          (!account || account.disabled || !account.hasKey)
        )
          throw new Error(
            "Select an enabled API account with a stored key before enabling paid fallback.",
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
          { type: "apiFallback", config },
          [
            ["Automatic paid fallback", config.enabled ? "Enabled" : "Off"],
            ["Provider", account?.label || "None"],
            ["Provider endpoint", account?.baseUrl || "None"],
            ["Model", account?.model || "None"],
            ["Subscription wait", `${config.waitMinutes} minutes`],
          ],
        );
      },
      "Review fallback",
    );
  }
  function showApiKey(account) {
    let key;
    openDialog(
      account.hasKey ? "Replace API key" : "Add API key",
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
          throw new Error("Enter a new API key before continuing.");
        openDialog(
          "Confirm API key replacement",
          "Replace the stored API key for the exact profile shown below. No request is sent to this provider to test the key.",
          (body) =>
            body.append(
              definitions([
                ["Account", account.label],
                ["Profile ID", account.id],
                ["Provider endpoint", account.baseUrl],
                ["New API key", "Entered; never displayed"],
              ]),
            ),
          async () => {
            try {
              await perform(payload);
            } finally {
              payload.apiKey = "";
              dialogCompleted = true;
              $("dialog-submit").disabled = true;
              $("dialog-cancel").textContent = "Close";
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
    const replacement =
      returnFocusKey &&
      [...$("app").querySelectorAll("button[data-focus-key]")].find(
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
      ? "Metadata updates are paused while this page is hidden."
      : "Account metadata is read every 5 seconds while this page is visible.";
    if (!document.hidden && state.paired) readInventory().catch(() => {});
  });
  $("language").value = messages.language();
  $("language").addEventListener("change", () => {
    if ($("action-dialog").open || state.busy) return;
    messages.setLanguage($("language").value);
    setPaired(state.paired);
    if (state.inventory) renderInventory();
    notice(state.paired ? "Account status is ready." : "Pairing required");
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
          "The manager returned an invalid pairing session. Pair this browser again.",
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
  try {
    const saved = JSON.parse(sessionStorage.getItem(resetStorageKey) || "null");
    if (
      saved &&
      [saved.profileId, saved.creditId, saved.idempotencyKey].every(
        (value) =>
          typeof value === "string" && value.length > 0 && value.length <= 256,
      )
    )
      state.redemption = saved;
  } catch {
    /* Pairing and browsing do not require session storage. */
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
