//! `/account` picker for the native multi-account pool.
//!
//! The picker lists every configured account profile with its scheduling
//! state and lets the user activate one (or return to automatic fill-first
//! scheduling). Activation goes through the app-server `accountPool/use`
//! RPC, so it drives the exact same scheduler used by model requests.

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountPoolAccount;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolRateLimitWindow;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::AccountPoolUseResponse;
use codex_app_server_protocol::AccountPoolWindowWarmup;
use codex_app_server_protocol::AccountPoolWindowWarmupOutcome;
use codex_config::AccountPoolRotationStrategy;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
use codex_login::format_reset_countdown;
use codex_login::format_window_warmup_status;
use ratatui::text::Span;

use super::*;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;

impl ChatWidget {
    pub(crate) fn open_account_pool_picker(
        &mut self,
        result: Result<AccountPoolReadResponse, String>,
    ) {
        let pool = match result {
            Ok(pool) => pool,
            Err(error) => {
                self.add_error_message(format!("Failed to read the account pool: {error}"));
                return;
            }
        };
        if !pool.enabled {
            self.add_info_message(
                "The multi-account pool is not configured. Add accounts with `codex account add`."
                    .to_string(),
                /*hint*/ None,
            );
            return;
        }

        let rotation_strategy = self.config_ref().account_pool.effective_rotation_strategy();
        let now = Utc::now();
        let mut items: Vec<SelectionItem> =
            Vec::with_capacity(pool.accounts.len() + rotation_strategy_items().len() + 1);
        for (strategy, name, description) in rotation_strategy_items() {
            let is_current = rotation_strategy == strategy;
            items.push(SelectionItem {
                name,
                description: Some(description),
                is_current,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::UpdateAccountPoolRotationStrategy { strategy });
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        let automatic_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::ActivateAccountPoolProfile {
                profile_id: None,
                force: true,
            });
        })];
        items.push(SelectionItem {
            name: "Automatic".to_string(),
            description: Some(
                "Let the scheduler pick the next eligible profile using the rotation strategy above."
                    .to_string(),
            ),
            actions: automatic_actions,
            dismiss_on_select: true,
            ..Default::default()
        });
        for account in &pool.accounts {
            let profile_id = account.profile_id.clone();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::ActivateAccountPoolProfile {
                    profile_id: Some(profile_id.clone()),
                    force: false,
                });
            })];
            items.push(SelectionItem {
                name: account_display_name(account),
                description_spans: account_description(account, now),
                is_current: account.is_active,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select Codex account".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    /// Shows the active pool profile in the `/status` account line. Runs after the
    /// `account/updated` refresh so the profile identity overlays the (email-less)
    /// auth-mode display instead of being overwritten by it.
    pub(crate) fn update_account_pool_identity(&mut self, active_profile: Option<String>) {
        if let Some(crate::status::StatusAccountDisplay::ChatGpt { email, .. }) =
            self.status_account_display.as_mut()
        {
            *email = active_profile;
        }
    }

    pub(crate) fn apply_account_pool_read_response(&mut self, pool: &AccountPoolReadResponse) {
        if pool.enabled {
            self.update_account_pool_identity(active_pool_profile_label(pool));
        }
    }

    pub(crate) fn on_account_pool_activated(
        &mut self,
        result: Result<AccountPoolUseResponse, String>,
    ) {
        match result {
            Ok(response) => {
                self.add_info_message(
                    format!(
                        "Switched to Codex account `{}`.",
                        response.active_profile_id
                    ),
                    /*hint*/ None,
                );
            }
            Err(error) => {
                self.add_error_message(format!("Failed to switch account: {error}"));
            }
        }
    }
}

fn account_display_name(account: &AccountPoolAccount) -> String {
    match (&account.label, &account.email) {
        (Some(label), _) => format!("{} ({label})", account.profile_id),
        (None, Some(email)) => format!("{} ({email})", account.profile_id),
        (None, None) => account.profile_id.clone(),
    }
}

fn account_description(account: &AccountPoolAccount, now: DateTime<Utc>) -> Vec<Span<'static>> {
    let mut parts: Vec<Vec<Span<'static>>> =
        vec![vec![format!("priority {}", account.priority).dim()]];
    if let Some(plan) = &account.plan_type {
        parts.push(vec![format!("{plan:?}").to_lowercase().dim()]);
    }
    parts.push(vec![match &account.availability {
        AccountPoolAvailability::Available => "available".dim(),
        AccountPoolAvailability::Exhausted { resets_at } => match resets_at {
            Some(resets_at) => {
                let remaining_seconds = (*resets_at).saturating_sub(now.timestamp());
                if remaining_seconds <= 0 {
                    "cooling down, reset now".dim()
                } else {
                    format!(
                        "cooling down, reset in {}",
                        format_reset_countdown(remaining_seconds as u64)
                    )
                    .dim()
                }
            }
            None => "cooling down".dim(),
        },
        AccountPoolAvailability::AuthenticationUnavailable { .. } => {
            "login broken; run `codex account login <id>`".dim()
        }
        AccountPoolAvailability::Disabled => "disabled".dim(),
    }]);
    if account.rate_limits.primary.is_none() && account.rate_limits.secondary.is_none() {
        parts.push(vec!["quota unknown".dim()]);
    }
    if let Some(primary) = &account.rate_limits.primary {
        parts.push(account_rate_limit_description(
            primary,
            AccountRateLimitKind::FiveHour,
            now,
        ));
    }
    if let Some(secondary) = &account.rate_limits.secondary {
        parts.push(account_rate_limit_description(
            secondary,
            AccountRateLimitKind::Weekly,
            now,
        ));
    }
    if let Some(warmup) = account.window_warmup.as_ref().and_then(|warmup| {
        if account.is_active {
            return None;
        }
        Some(format_window_warmup_status(
            &protocol_warmup_to_login(warmup),
            now,
        ))
    }) {
        parts.push(vec![warmup.dim()]);
    }

    let mut description = Vec::new();
    for part in parts {
        if !description.is_empty() {
            description.push(" · ".dim());
        }
        description.extend(part);
    }
    description
}

#[derive(Clone, Copy)]
enum AccountRateLimitKind {
    FiveHour,
    Weekly,
}

fn account_rate_limit_description(
    window: &AccountPoolRateLimitWindow,
    kind: AccountRateLimitKind,
    now: DateTime<Utc>,
) -> Vec<Span<'static>> {
    let colorize = |text: String| match kind {
        AccountRateLimitKind::FiveHour => text.cyan(),
        AccountRateLimitKind::Weekly => text.magenta(),
    };
    let label = match kind {
        AccountRateLimitKind::FiveHour => " 5h used",
        AccountRateLimitKind::Weekly => " weekly used",
    };
    let mut spans = vec![
        colorize(format!("{:.0}%", window.used_percent)),
        label.dim(),
    ];
    // A 0% primary window has not started ticking; the backend still reports a full-window
    // reset which would otherwise look stuck at ~5h until the first real request.
    if matches!(kind, AccountRateLimitKind::FiveHour) && window.used_percent <= 0.0 {
        spans.push(", ".dim());
        spans.push(colorize("not started".to_string()));
        return spans;
    }
    if let Some(resets_at) = window.resets_at {
        let remaining_seconds = resets_at.saturating_sub(now.timestamp());
        let (prefix, countdown) = if remaining_seconds <= 0 {
            (", reset ", "now".to_string())
        } else {
            (
                ", reset in ",
                format_reset_countdown(remaining_seconds as u64),
            )
        };
        spans.push(prefix.dim());
        spans.push(colorize(countdown));
    }
    spans
}

fn rotation_strategy_items() -> [(AccountPoolRotationStrategy, String, String); 2] {
    [
        (
            AccountPoolRotationStrategy::FillFirst,
            "Rotation: fill-first".to_string(),
            "Prefer the lowest priority among eligible profiles.".to_string(),
        ),
        (
            AccountPoolRotationStrategy::EarliestReset,
            "Rotation: earliest-reset".to_string(),
            "Prefer due cooldowns, idle (not-yet-started) 5h windows, then the soonest reset."
                .to_string(),
        ),
    ]
}

fn protocol_warmup_to_login(warmup: &AccountPoolWindowWarmup) -> WindowWarmupObservation {
    WindowWarmupObservation {
        outcome: match warmup.outcome {
            AccountPoolWindowWarmupOutcome::Succeeded => WindowWarmupOutcome::Succeeded,
            AccountPoolWindowWarmupOutcome::Failed => WindowWarmupOutcome::Failed,
            AccountPoolWindowWarmupOutcome::SkippedNoAuth => WindowWarmupOutcome::SkippedNoAuth,
        },
        attempted_at: DateTime::<Utc>::from_timestamp(warmup.attempted_at, 0)
            .unwrap_or_else(Utc::now),
        retry_after: warmup
            .retry_after
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
    }
}

pub(crate) fn active_pool_profile_label(pool: &AccountPoolReadResponse) -> Option<String> {
    pool.accounts
        .iter()
        .find(|account| account.is_active)
        .map(|account| {
            account
                .label
                .clone()
                .unwrap_or_else(|| account.profile_id.clone())
        })
}

#[cfg(test)]
#[path = "account_popups_tests.rs"]
mod tests;
