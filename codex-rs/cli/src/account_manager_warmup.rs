//! Human-readable warmup evidence, separate from quota refresh and reset credits.

use super::Locale;
use codex_app_server::account_management::ManagedAccountView;

pub(super) fn render(account: &ManagedAccountView, locale: Locale) -> String {
    let mut lines = vec![locale.text("Standby warmup").to_string()];
    lines.push(locale.text("Warmup uses a small generating request. Viewing this page and refreshing quota do not start warmup.").into());
    if let Some(view) = &account.warmup {
        lines.push(locale.format("Latest evidence: {}", &[status(&view.status, locale)]));
        if let Some(at) = chrono::DateTime::from_timestamp(view.attempted_at, /*nsecs*/ 0) {
            lines.push(locale.format(
                "Last attempt: {}",
                &[&at.format("%Y-%m-%d %H:%M UTC").to_string()],
            ));
        }
        if let Some(retry) = view
            .retry_after
            .and_then(|at| chrono::DateTime::from_timestamp(at, /*nsecs*/ 0))
        {
            lines.push(locale.format(
                "Next eligible check: {}",
                &[&retry.format("%Y-%m-%d %H:%M UTC").to_string()],
            ));
        }
        if view.consecutive_failures > 0 {
            lines.push(locale.format(
                "Consecutive failures: {}",
                &[&view.consecutive_failures.to_string()],
            ));
        }
    } else {
        lines.push(locale.text("No recent warmup attempt recorded. A quota window may also start during normal use.").into());
    }
    lines.push(locale.text("A completed request or reset timestamp alone does not confirm a quota window started; positive current usage does.").into());
    lines.join("\n")
}

fn status(value: &str, locale: Locale) -> &'static str {
    locale.text(match value {
        "windowActive" => "Current quota confirms the window is active",
        "inProgress" => "Warmup request is running",
        "unconfirmed" => "Request sent; window start unconfirmed",
        "completedUnconfirmed" => "Request completed; window start unconfirmed",
        "needsLogin" => "Login is needed before warmup",
        "deferred" => "Deferred until the next eligible check",
        "failed" => "Request failed; waiting before retry",
        "retryReady" => "A retry is eligible when the scheduler runs",
        "expired" => "Earlier evidence has expired",
        _ => "No confirmed warmup evidence",
    })
}

#[cfg(test)]
#[path = "account_manager_warmup_tests.rs"]
mod tests;
