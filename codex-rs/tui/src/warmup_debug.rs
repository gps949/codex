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
            lines.push(format!("  {}", format_account_line(account)).into());
        }
    }

    lines.push("".into());
    lines.push("Events (oldest first):".bold().into());
    if response.events.is_empty() {
        lines.push("  <none>".dim().into());
        lines.push(
            "  First automatic pass waits for settle, then warms one idle standby per interval."
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
        "Picker hides warmup status. Success is 5h used% > 0. /warmup now requests one pass immediately."
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
        .email
        .as_deref()
        .or(account.label.as_deref())
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
    format!(
        "{name} {role} pri={} {} 5h={used} candidate={candidate} persisted={persisted} id={}",
        account.priority, account.availability, account.profile_id
    )
}

fn format_event_line(event: &AccountPoolWarmupDebugEvent) -> String {
    let at = DateTime::<Utc>::from_timestamp(event.at, 0)
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| format!("unix:{}", event.at));
    format!("{at}  {}", event.message)
}

#[cfg(test)]
#[path = "warmup_debug_tests.rs"]
mod tests;
