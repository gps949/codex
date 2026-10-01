//! Transcript dump for the `/warmup` slash command.

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountPoolWarmupDebugAccount;
use codex_app_server_protocol::AccountPoolWarmupDebugEvent;
use codex_app_server_protocol::AccountPoolWarmupDebugResponse;
use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::history_cell::PlainHistoryCell;

pub(crate) fn new_warmup_debug_output(
    response: &AccountPoolWarmupDebugResponse,
) -> PlainHistoryCell {
    PlainHistoryCell::new(render_warmup_debug_lines(response))
}

fn render_warmup_debug_lines(response: &AccountPoolWarmupDebugResponse) -> Vec<Line<'static>> {
    let mut lines = vec!["/warmup".magenta().into(), "".into()];
    lines.push("Config:".bold().into());
    lines.push(format!("  window_warmup = {}", response.enabled).into());
    lines.push(
        format!(
            "  pool_enabled = {}",
            response
                .pool_enabled
                .map(|enabled| enabled.to_string())
                .as_deref()
                .unwrap_or("not reported")
        )
        .into(),
    );
    lines.push(format!("  task_running = {}", response.task_running).into());
    lines.push(format!("  pass_requested = {}", response.pass_requested).into());
    lines.push(format!("  interval = {}s", response.interval_seconds).into());
    lines.push(format!("  settle = {}s", response.settle_seconds).into());
    lines.push(format!("  rotation_strategy = {}", response.rotation_strategy).into());
    lines.push(
        format!(
            "  session_model = {}",
            response.session_model.as_deref().unwrap_or("<unset>")
        )
        .into(),
    );

    lines.push("".into());
    lines.push("Accounts:".bold().into());
    if response.accounts.is_empty() {
        lines.push("  <none>".dim().into());
    } else {
        for account in &response.accounts {
            for line in format_account_line(account).lines() {
                lines.push(format!("  {line}").into());
            }
        }
    }

    lines.push("".into());
    lines.push("Events (this process; oldest first):".bold().into());
    if response.events.is_empty() {
        lines.push("  <none>".dim().into());
        lines.push(
            if response.enabled {
                "  First automatic pass waits for settle, then warms one idle standby per interval."
            } else {
                "  Automatic warmup is off. Enable it before requesting a pass."
            }
            .dim()
            .into(),
        );
    } else {
        for event in &response.events {
            lines.push(format!("  {}", format_event_line(event)).into());
        }
    }

    lines.push("".into());
    lines.push(
        "0% does not confirm a 5h start. Weekly resets are separate. Persisted attempts survive restarts."
            .dim()
            .into(),
    );
    lines.push(
        "/warmup now checks one standby; it respects off/backoff and may use quota."
            .dim()
            .into(),
    );
    if response.pass_requested {
        lines.push("Pass requested; run /warmup again after it finishes.".into());
    }
    lines
}

fn format_account_line(account: &AccountPoolWarmupDebugAccount) -> String {
    let name = account
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .or(account.email.as_deref())
        .unwrap_or(account.profile_id.as_str());
    let role = if account.is_active {
        "current"
    } else {
        "standby"
    };
    let candidate = if account.is_candidate { "yes" } else { "no" };
    let used = account
        .primary_used_percent
        .map(|used| format!("{used}%"))
        .unwrap_or_else(|| "unknown".to_string());
    let persisted = account
        .persisted_warmup_outcome
        .as_deref()
        .unwrap_or("none");
    let status = account.status.as_deref().unwrap_or("not reported");
    let reason = account
        .candidate_reason
        .as_deref()
        .unwrap_or("not reported");
    let attempted = account
        .attempted_at
        .map(format_timestamp)
        .unwrap_or_else(|| "none".to_string());
    let retry = account
        .retry_after
        .map(format_timestamp)
        .unwrap_or_else(|| "none".to_string());
    format!(
        "{name} {role} pri={} {} 5h={used} candidate={candidate}\n    reason={reason}; warmup={status}\n    attempted={attempted}; retry_after={retry}; persisted={persisted}; id={}",
        account.priority, account.availability, account.profile_id
    )
}

fn format_event_line(event: &AccountPoolWarmupDebugEvent) -> String {
    let at = format_timestamp(event.at);
    format!("{at}  {}", event.message)
}

fn format_timestamp(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("unix:{timestamp}"))
}

#[cfg(test)]
#[path = "warmup_debug_tests.rs"]
mod tests;
