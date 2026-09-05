//! `/account` picker for the native multi-account pool.
//!
//! The picker lists every configured account profile with its scheduling
//! state and lets the user activate one (or return to automatic fill-first
//! scheduling). Activation goes through the app-server `accountPool/use`
//! RPC, so it drives the exact same scheduler used by model requests.

use codex_app_server_protocol::AccountPoolAccount;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::AccountPoolUseResponse;
use codex_config::AccountPoolRotationStrategy;
use codex_login::format_exhausted_reset_unix;

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
            tx.send(AppEvent::ActivateAccountPoolProfile { profile_id: None });
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
                });
            })];
            items.push(SelectionItem {
                name: account_display_name(account),
                description: Some(account_description(account)),
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

fn account_description(account: &AccountPoolAccount) -> String {
    let mut parts = vec![format!("priority {}", account.priority)];
    if let Some(plan) = &account.plan_type {
        parts.push(format!("{plan:?}").to_lowercase());
    }
    parts.push(match &account.availability {
        AccountPoolAvailability::Available => "available".to_string(),
        AccountPoolAvailability::Exhausted { resets_at } => match resets_at {
            Some(resets_at) => format!(
                "cooling down until {}",
                format_exhausted_reset_unix(*resets_at)
            ),
            None => "cooling down".to_string(),
        },
        AccountPoolAvailability::AuthenticationUnavailable { .. } => {
            "login broken; run `codex account login <id>`".to_string()
        }
        AccountPoolAvailability::Disabled => "disabled".to_string(),
    });
    if account.rate_limits.primary.is_none() && account.rate_limits.secondary.is_none() {
        parts.push("quota unknown".to_string());
    }
    if let Some(primary) = &account.rate_limits.primary {
        parts.push(format!("{:.0}% of 5h window used", primary.used_percent));
    }
    if let Some(secondary) = &account.rate_limits.secondary {
        parts.push(format!(
            "{:.0}% of weekly window used",
            secondary.used_percent
        ));
    }
    match account
        .rate_limits
        .observed_at
        .and_then(|time| chrono::DateTime::from_timestamp(time, 0))
    {
        Some(time) => parts.push(format!("checked {} UTC", time.format("%m-%d %H:%M"))),
        None if account.rate_limits.primary.is_some()
            || account.rate_limits.secondary.is_some() =>
        {
            parts.push("cached; check time unknown".to_string())
        }
        None => {}
    }
    parts.join(" · ")
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
            "Prefer the profile whose rate-limit window resets soonest.".to_string(),
        ),
    ]
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
