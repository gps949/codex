"use strict";

(() => {
  const { t } = window.AccountManagerMessages;
  const labels = {
    windowActive: "Current quota confirms the window is active",
    inProgress: "Warmup request is running",
    unconfirmed: "Request sent; window start unconfirmed",
    completedUnconfirmed: "Request completed; window start unconfirmed",
    needsLogin: "Login is needed before warmup",
    deferred: "Deferred until the next eligible check",
    failed: "Request failed; waiting before retry",
    retryReady: "A retry is eligible when the scheduler runs",
    expired: "Earlier evidence has expired",
  };
  function render(account, ui) {
    const body = ui.element("div");
    body.append(
      ui.element(
        "p",
        t(
          "Warmup uses a small generating request. Viewing this page and refreshing quota do not start warmup.",
        ),
        "muted",
      ),
    );
    if (account.warmup) {
      const view = account.warmup;
      body.append(
        ui.definitions([
          [
            "Latest evidence",
            t(
              Object.hasOwn(labels, view.status)
                ? labels[view.status]
                : "No confirmed warmup evidence",
            ),
          ],
          ["Last attempt", ui.date(view.attemptedAt)],
          ["Next eligible check", ui.date(view.retryAfter)],
        ]),
      );
      if (view.consecutiveFailures > 0)
        body.append(
          ui.element(
            "p",
            t("Consecutive failures: {count}", {
              count: view.consecutiveFailures,
            }),
          ),
        );
    } else {
      body.append(
        ui.element(
          "p",
          t(
            "No recent warmup attempt recorded. A quota window may also start during normal use.",
          ),
        ),
      );
    }
    body.append(
      ui.element(
        "small",
        t(
          "A completed request or reset timestamp alone does not confirm a quota window started; positive current usage does.",
        ),
      ),
    );
    return body;
  }
  window.AccountManagerWarmup = Object.freeze({ render });
})();
