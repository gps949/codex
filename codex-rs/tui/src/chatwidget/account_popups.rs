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

        let mut items: Vec<SelectionItem> = Vec::with_capacity(pool.accounts.len() + 1);
        let automatic_actions: Vec<SelectionAction> = vec![Box::new(|tx| {
            tx.send(AppEvent::ActivateAccountPoolProfile { profile_id: None });
        })];
        items.push(SelectionItem {
            name: "Automatic".to_string(),
            description: Some(
                "Fill-first scheduling: prefer the lowest-priority eligible account".to_string(),
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
            Some(resets_at) => format!("cooling down until {resets_at}"),
            None => "cooling down".to_string(),
        },
        AccountPoolAvailability::AuthenticationUnavailable { .. } => {
            "login broken; run `codex account login <id>`".to_string()
        }
        AccountPoolAvailability::Disabled => "disabled".to_string(),
    });
    if let Some(primary) = &account.rate_limits.primary {
        parts.push(format!("{:.0}% of 5h window used", primary.used_percent));
    }
    if let Some(secondary) = &account.rate_limits.secondary {
        parts.push(format!(
            "{:.0}% of weekly window used",
            secondary.used_percent
        ));
    }
    parts.join(" · ")
}
