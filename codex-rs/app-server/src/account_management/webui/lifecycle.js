"use strict";

(() => {
  let token = "";
  let clientId = crypto.randomUUID();
  let timer = null;
  let pending = false;
  let suspended = false;
  let stopped = false;

  async function heartbeat() {
    if (!token || pending || suspended || stopped) return;
    clearTimeout(timer);
    const capturedToken = token;
    const capturedId = clientId;
    pending = true;
    try {
      await fetch("/api/heartbeat", {
        method: "POST",
        credentials: "omit",
        redirect: "error",
        cache: "no-store",
        headers: {
          "Content-Type": "application/json",
          "x-codex-pool-token": capturedToken,
        },
        body: JSON.stringify({ clientId: capturedId }),
        signal: AbortSignal.timeout(10000),
      });
    } catch {
      // Temporary network loss must not spin or interrupt a running operation.
    } finally {
      pending = false;
      if (token && !suspended && !stopped) {
        if (token === capturedToken && clientId === capturedId)
          timer = setTimeout(heartbeat, 30000);
        else heartbeat();
      }
    }
  }

  function disconnect() {
    clearTimeout(timer);
    token = "";
  }

  window.addEventListener("pagehide", () => {
    clearTimeout(timer);
    suspended = true;
    if (!token || stopped) return;
    // Keepalive is best effort. The host lease also expires if this request is lost.
    fetch("/api/leave", {
      method: "POST",
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      keepalive: true,
      headers: {
        "Content-Type": "application/json",
        "x-codex-pool-token": token,
      },
      body: JSON.stringify({ clientId }),
    }).catch(() => {});
  });
  window.addEventListener("pageshow", (event) => {
    if (!event.persisted || stopped) return;
    clientId = crypto.randomUUID();
    suspended = false;
    heartbeat();
  });
  document.addEventListener("visibilitychange", heartbeat);

  window.AccountManagerLifecycle = Object.freeze({
    connect(sessionToken) {
      if (stopped || token === sessionToken) return;
      token = sessionToken;
      heartbeat();
    },
    disconnect,
    stop() {
      stopped = true;
      disconnect();
    },
  });
})();
