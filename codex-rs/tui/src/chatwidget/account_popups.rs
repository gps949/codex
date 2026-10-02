//! `/account` picker for the native multi-account pool.
//!
//! Accounts, Strategy, and Help tabs separate the next account choice from
//! automatic scheduling and explain the current quota-saving settings.
//! Choosing a profile uses the app-server `accountPool/use` RPC; automatic
//! failover remains enabled for subsequent model requests.

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AccountPoolAccount;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolRateLimitWindow;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::AccountPoolUpdatedNotification;
use codex_app_server_protocol::AccountPoolUseResponse;
use codex_app_server_protocol::AccountPoolWindowWarmup;
use codex_app_server_protocol::AccountPoolWindowWarmupOutcome;
use codex_app_server_protocol::AccountPoolWindowWarmupPhase;
use codex_config::AccountPoolRotationStrategy;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
use codex_login::WindowWarmupPhase;
use codex_login::format_reset_countdown;
use codex_login::visible_window_warmup_status;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use super::*;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionTab;
use crate::bottom_pane::SelectionViewParams;
use crate::keymap::ListAction;

#[path = "account_pool_columns.rs"]
mod columns;
#[path = "account_pool_help.rs"]
mod help;

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

        let config = &self.config_ref().account_pool;
        let rotation_strategy = config.effective_rotation_strategy();
        let now = Utc::now();
        let mut strategy_items = Vec::new();
        for (strategy, name, description) in rotation_strategy_items() {
            let is_current = rotation_strategy == strategy;
            strategy_items.push(SelectionItem {
                search_value: Some(format!("{strategy:?} {name} {description}")),
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
        strategy_items.sort_by_key(|item| !item.is_current);
        let initial_selected_idx = pool.accounts.iter().position(|account| {
            account.is_active
                && !matches!(
                    account.availability,
                    AccountPoolAvailability::Disabled
                        | AccountPoolAvailability::AuthenticationUnavailable { .. }
                )
        });
        let column_widths = columns::AccountColumnWidths::new(&pool.accounts, now);
        let mut account_items = Vec::with_capacity(pool.accounts.len() + 1);
        for account in &pool.accounts {
            let profile_id = account.profile_id.clone();
            let retry = matches!(
                account.availability,
                AccountPoolAvailability::Exhausted { .. }
            );
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::ActivateAccountPoolProfile {
                    profile_id: Some(profile_id.clone()),
                    force: retry,
                });
            })];
            let disabled_reason = match account.availability {
                AccountPoolAvailability::Disabled => Some(format!(
                    "Enable with `codex account enable {profile_id}`",
                    profile_id = account.profile_id
                )),
                AccountPoolAvailability::AuthenticationUnavailable { .. } => Some(format!(
                    "Sign in again with `codex account login {profile_id}`",
                    profile_id = account.profile_id
                )),
                AccountPoolAvailability::Available | AccountPoolAvailability::Exhausted { .. } => {
                    None
                }
            };
            account_items.push(SelectionItem {
                name: if retry {
                    format!("Retry {}", account_display_name(account))
                } else {
                    account_display_name(account)
                },
                description_spans: column_widths.description(account, now),
                selected_description: retry.then(|| {
                    "Clear the local cooldown and retry this account. The server's quota limit still applies."
                        .to_string()
                }),
                search_value: Some(format!(
                    "{} {} {}",
                    account_display_name(account),
                    account.profile_id,
                    account.email.as_deref().unwrap_or_default()
                )),
                is_current: account.is_active,
                disabled_reason,
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        let automatic_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::ActivateAccountPoolProfile {
                profile_id: None,
                force: false,
            });
        })];
        account_items.push(SelectionItem {
            name: "Choose automatically".to_string(),
            description: Some("Pick an eligible account using the Strategy tab".to_string()),
            search_value: Some("automatic scheduler".to_string()),
            actions: automatic_actions,
            dismiss_on_select: true,
            ..Default::default()
        });

        let help_items = help::items(config);
        let mut hint = Vec::new();
        let keymap = self.bottom_pane.list_keymap();
        for (action, label) in [
            (ListAction::MoveLeft, "previous tab"),
            (ListAction::MoveRight, "next tab"),
            (ListAction::Accept, "select"),
            (ListAction::Cancel, "back"),
        ] {
            if let Some(key) = keymap.primary_hint(action) {
                if !hint.is_empty() {
                    hint.push(" · ".dim());
                }
                hint.extend(key.spans());
                hint.push(format!(" {label}").dim());
            }
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            footer_hint: Some(hint.into()),
            tabs: vec![
                SelectionTab {
                    id: "accounts".to_string(),
                    label: format!("Accounts ({})", pool.accounts.len()),
                    header: account_picker_header(
                        "Account pool",
                        "Choose an account to use next. Automatic failover stays enabled.",
                    ),
                    items: account_items,
                },
                SelectionTab {
                    id: "strategy".to_string(),
                    label: "Strategy".to_string(),
                    header: account_picker_header(
                        "Automatic selection",
                        "Both strategies use eligible accounts and preserve reserve quota.",
                    ),
                    items: strategy_items,
                },
                SelectionTab {
                    id: "help".to_string(),
                    label: "Help".to_string(),
                    header: account_picker_header(
                        "How your pool works",
                        "Review current settings and quota-saving behavior.",
                    ),
                    items: help_items,
                },
            ],
            is_searchable: true,
            search_placeholder: Some("Search accounts or settings".to_string()),
            initial_selected_idx,
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

    /// Refresh identity-bound account data once per activation, while quota observations
    /// continue updating the visible label without restarting network reads.
    pub(crate) fn on_account_pool_updated(
        &mut self,
        pool: &AccountPoolUpdatedNotification,
    ) -> bool {
        let identity = (pool.active_profile_id.clone(), pool.active_generation);
        let changed = self.account_pool_identity.as_ref() != Some(&identity);
        self.account_pool_identity = Some(identity);
        let active_profile = pool
            .accounts
            .iter()
            .find(|account| account.is_active)
            .map(account_display_name);
        self.update_account_pool_identity(active_profile);
        changed
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

pub(crate) fn account_display_name(account: &AccountPoolAccount) -> String {
    // Labels distinguish personal and workspace profiles that share the same email.
    account
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .or_else(|| {
            account
                .email
                .as_deref()
                .map(str::trim)
                .filter(|email| !email.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| account.profile_id.clone())
}

fn account_description_parts(
    account: &AccountPoolAccount,
    now: DateTime<Utc>,
) -> Vec<Vec<Span<'static>>> {
    let mut parts = vec![vec![match &account.availability {
        AccountPoolAvailability::Available => "ready".green(),
        AccountPoolAvailability::Exhausted { resets_at } => match resets_at {
            Some(resets_at) => {
                let deadline_label = if account.backend_resets_at.is_some() {
                    "reset"
                } else {
                    "retry"
                };
                let remaining_seconds = (*resets_at).saturating_sub(now.timestamp());
                if remaining_seconds <= 0 {
                    format!("cooling down, {deadline_label} now").dim()
                } else {
                    format!(
                        "cooling down, {deadline_label} in {}",
                        format_reset_countdown(remaining_seconds as u64)
                    )
                    .dim()
                }
            }
            None => "cooling down".dim(),
        },
        AccountPoolAvailability::AuthenticationUnavailable { .. } => "needs login".red(),
        AccountPoolAvailability::Disabled => "disabled".dim(),
    }]];
    if let Some(primary) = &account.rate_limits.primary {
        parts.push(account_rate_limit_description(
            primary,
            AccountRateLimitKind::FiveHour,
            now,
        ));
    } else {
        parts.push(vec!["  -- 5h left, unknown".dim()]);
    }
    if let Some(secondary) = &account.rate_limits.secondary {
        parts.push(account_rate_limit_description(
            secondary,
            AccountRateLimitKind::Weekly,
            now,
        ));
    } else {
        parts.push(vec!["  -- weekly left, unknown".dim()]);
    }
    if let Some(plan) = &account.plan_type {
        parts.push(vec![format!("{plan:?}").to_lowercase().dim()]);
    } else {
        parts.push(vec!["plan ?".dim()]);
    }
    parts.push(vec!["priority ".dim(), account.priority.to_string().dim()]);
    for (name, window, observed_at) in [
        (
            "5h",
            &account.rate_limits.primary,
            account.rate_limits.primary_observed_at,
        ),
        (
            "weekly",
            &account.rate_limits.secondary,
            account.rate_limits.secondary_observed_at,
        ),
    ] {
        if window.is_none() {
            continue;
        }
        let Some(observed_at) = observed_at else {
            continue;
        };
        let age_minutes = now.timestamp().saturating_sub(observed_at) / 60;
        if age_minutes >= 15 {
            let age = if age_minutes >= 24 * 60 {
                format!("{}d", age_minutes / (24 * 60))
            } else if age_minutes >= 60 {
                format!("{}h", age_minutes / 60)
            } else {
                format!("{age_minutes}m")
            };
            parts.push(vec![format!("{name} cached {age} ago").dim()]);
        }
    }

    if let Some(email) = account
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty() && *email != account_display_name(account))
    {
        parts.push(vec![email.to_string().dim()]);
    }
    if let Some(warmup) = account
        .window_warmup
        .as_ref()
        .filter(|_| !account.is_active)
        .and_then(|warmup| {
            visible_window_warmup_status(
                &protocol_warmup_to_login(warmup),
                account
                    .rate_limits
                    .primary
                    .as_ref()
                    .map(|window| window.used_percent),
                now,
            )
        })
    {
        parts.push(vec![warmup.dim()]);
    }

    parts
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
        AccountRateLimitKind::FiveHour => " 5h left",
        AccountRateLimitKind::Weekly => " weekly left",
    };
    let mut spans = vec![
        colorize(format!(
            "{:>3.0}%",
            (100.0 - window.used_percent).clamp(0.0, 100.0)
        )),
        label.dim(),
    ];
    // Zero usage can also follow a completed tiny request. The backend's full-window
    // reset alone does not confirm that a primary clock has started.
    if matches!(kind, AccountRateLimitKind::FiveHour) && window.used_percent <= 0.0 {
        spans.push(", ".dim());
        spans.push(colorize("start unconfirmed".to_string()));
        return spans;
    }
    if let Some(resets_at) = window.resets_at {
        let remaining_seconds = resets_at.saturating_sub(now.timestamp());
        let (prefix, countdown) = if remaining_seconds <= 0 {
            (", reset ", "now".to_string())
        } else {
            (", reset ", format_reset_countdown(remaining_seconds as u64))
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
            "By priority".to_string(),
            "Use the lowest priority number first; keep others for failover".to_string(),
        ),
        (
            AccountPoolRotationStrategy::EarliestReset,
            "By reset time".to_string(),
            "Start idle 5h windows early, then prefer the soonest reset".to_string(),
        ),
    ]
}

fn account_picker_header(
    title: &str,
    subtitle: &str,
) -> Box<dyn crate::render::renderable::Renderable> {
    Box::new(
        Paragraph::new(vec![
            Line::from(title.to_string().bold()),
            Line::from(subtitle.to_string().dim()),
        ])
        .wrap(Wrap { trim: false }),
    )
}

fn protocol_warmup_to_login(warmup: &AccountPoolWindowWarmup) -> WindowWarmupObservation {
    WindowWarmupObservation {
        outcome: match warmup.outcome {
            AccountPoolWindowWarmupOutcome::Succeeded => WindowWarmupOutcome::Succeeded,
            AccountPoolWindowWarmupOutcome::Failed => WindowWarmupOutcome::Failed,
            AccountPoolWindowWarmupOutcome::SkippedNoAuth => WindowWarmupOutcome::SkippedNoAuth,
        },
        phase: warmup.phase.map(|phase| match phase {
            AccountPoolWindowWarmupPhase::InProgress => WindowWarmupPhase::InProgress,
            AccountPoolWindowWarmupPhase::Unconfirmed => WindowWarmupPhase::Unconfirmed,
        }),
        attempted_at: DateTime::<Utc>::from_timestamp(warmup.attempted_at, 0)
            .unwrap_or_else(Utc::now),
        retry_after: warmup
            .retry_after
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
        // Wire protocol omits streak; explicit phase preserves attempt semantics.
        consecutive_failures: warmup.consecutive_failures.unwrap_or(1),
        request_generation: 0,
    }
}

pub(crate) fn active_pool_profile_label(pool: &AccountPoolReadResponse) -> Option<String> {
    pool.accounts
        .iter()
        .find(|account| account.is_active)
        .map(account_display_name)
}

#[cfg(test)]
#[path = "account_popups_tests.rs"]
mod tests;
