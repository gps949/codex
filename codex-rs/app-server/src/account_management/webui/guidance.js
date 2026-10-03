"use strict";

(() => {
  const t = (message, values) =>
    window.AccountManagerMessages.t(message, values);
  const names = {
    ready: "Eligible (cached)",
    coolingDown: "Cooling down",
    needsLogin: "Needs login",
    disabled: "Disabled",
    paused: "Paused",
    pending: "Login pending",
    signedIn: "Signed in",
    waiting: "Waiting",
    completed: "Completed",
    cancelled: "Cancelled",
    failed: "Failed",
  };
  let clockOffset = 0;
  function now() {
    return Date.now() / 1000 + clockOffset;
  }
  function setClock(unixSeconds) {
    clockOffset = Number.isFinite(unixSeconds)
      ? unixSeconds - Date.now() / 1000
      : 0;
  }
  function status(value) {
    return Object.hasOwn(names, value) ? t(names[value]) : value;
  }
  function duration(seconds) {
    const minutes = Math.max(1, Math.ceil(seconds / 60));
    if (minutes < 60) return t("{minutes}m", { minutes });
    const hours = Math.floor(minutes / 60);
    if (hours < 24)
      return t("{hours}h {minutes}m", { hours, minutes: minutes % 60 });
    return t("{days}d {hours}h", {
      days: Math.floor(hours / 24),
      hours: hours % 24,
    });
  }
  function age(observedAt) {
    if (!Number.isFinite(observedAt)) return t("Cache age unknown");
    const elapsed = now() - observedAt;
    if (elapsed < -60) return t("Cache clock skew; check the host clock");
    if (elapsed < 60) return t("Cached just now");
    return t("Cached {duration} ago", {
      duration: duration(Math.floor(elapsed / 60) * 60),
    });
  }
  function reset(window, name) {
    if (!window?.resetsAt) return t("Reset time unknown");
    if (name === "primary" && !(window.usedPercent > 0))
      return t("Window start unconfirmed");
    const seconds = window.resetsAt - now();
    return seconds > 0
      ? t("Resets in {duration}", { duration: duration(seconds) })
      : t("Reset time passed; check quota");
  }
  function action(account) {
    if (account.disabled) return { label: t("Enable account"), type: "enable" };
    if (account.loginState !== "signedIn")
      return { label: t("Complete login"), type: "login" };
    if (account.availability === "coolingDown")
      return { label: t("Refresh quota"), type: "refresh" };
    return { label: t("Use account"), type: "use" };
  }
  function reason(account) {
    if (account.disabled)
      return t(
        "This account is disabled. Enable it before use or quota checks.",
      );
    if (account.loginState !== "signedIn")
      return t("Complete login before use or quota checks.");
    if (account.availability === "coolingDown")
      return t(
        "Already reset elsewhere? Refresh quota to check for recovery without spending a credit.",
      );
    return t(
      "Eligible uses cached login and cooldown status. The next request confirms availability.",
    );
  }
  function summary(inventory) {
    const accounts = inventory.accounts || [];
    const jobs = inventory.loginJobs || [];
    const counts = {
      eligible: accounts.filter((account) => account.availability === "ready")
        .length,
      login: accounts.filter(
        (account) => !account.disabled && account.loginState !== "signedIn",
      ).length,
      cooling: accounts.filter(
        (account) => account.availability === "coolingDown",
      ).length,
    };
    let message = t(
      "Each account keeps its own quota. Cached eligibility is checked again on use.",
    );
    let next = null;
    if (inventory.apiSelection?.type === "manual") {
      message = t(
        "Manual API mode can incur provider charges. Return to subscriptions to use your subscription pool.",
      );
      next = { type: "automatic", label: t("Return to subscriptions") };
    } else if (!accounts.length) {
      message = t(
        "Add your first subscription account, then follow the verification link shown here.",
      );
      next = { type: "add", label: t("Add account") };
    } else if (inventory.paused) {
      message = t(
        "The pool is paused. Return to subscriptions to resume scheduling; this does not spend a reset credit.",
      );
      next = { type: "automatic", label: t("Return to subscriptions") };
    } else if (jobs.some((job) => job.status === "waiting")) {
      message = t(
        "A login is waiting for browser verification. Open its link and enter the code below.",
      );
      next = { type: "loginActivity", label: t("View login activity") };
    } else if (counts.cooling && !counts.eligible) {
      message = t(
        "The signed-in pool is cooling down. Check for restored quota after an external reset; no credit is spent by this check.",
      );
      next = { type: "refresh", label: t("Check for restored quota") };
    } else if (counts.login && !counts.eligible) {
      message = t(
        "Complete login for an account to make it eligible for the pool.",
      );
      next = {
        type: "login",
        profileId: accounts.find(
          (account) => !account.disabled && account.loginState !== "signedIn",
        ).profileId,
        label: t("Complete login"),
      };
    } else if (accounts.every((account) => account.disabled)) {
      message = t(
        "All subscription accounts are disabled. Open an account to enable it.",
      );
      next = {
        type: "details",
        profileId: accounts[0].profileId,
        label: t("Manage an account"),
      };
    }
    return { counts, message, next };
  }
  function creditScope(value) {
    return t(
      value === "codex_rate_limits"
        ? "Codex quota; the backend determines affected windows"
        : "Provider-defined scope; review technical details",
    );
  }
  window.AccountManagerGuidance = Object.freeze({
    names,
    status,
    now,
    setClock,
    duration,
    age,
    reset,
    action,
    reason,
    summary,
    creditScope,
  });
})();
