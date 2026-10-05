use super::usage::RATE_LIMIT_RESET_CONFIRMATION_VIEW_ID;
use super::usage::RATE_LIMIT_RESET_VIEW_ID;
use super::usage::usage_hint_line;
use super::*;
use crate::app::reset_credit_operation::ResetCreditOperation;
use crate::app::reset_credit_operation::valid_reset_owner_key;
use crate::clock_format::ClockFormat;
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_app_server_protocol::RateLimitResetCreditStatus;
use codex_app_server_protocol::RateLimitResetCreditsSummary;
use codex_app_server_protocol::RateLimitResetType;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ResetCreditOption {
    pub(super) credit_id: Option<String>,
    pub(super) name: String,
    pub(super) detail: Option<String>,
    pub(super) description: String,
}

pub(super) fn reset_credit_options(
    summary: &RateLimitResetCreditsSummary,
    clock_format: ClockFormat,
) -> Vec<ResetCreditOption> {
    let available_count = summary.available_count.max(0);
    let detail_limit = usize::try_from(available_count).unwrap_or(usize::MAX);
    let now = Utc::now().timestamp();
    let mut available_credits = summary
        .credits
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|credit| {
            credit.status == RateLimitResetCreditStatus::Available
                && credit.reset_type == RateLimitResetType::CodexRateLimits
                && credit.expires_at.is_none_or(|expires| expires > now)
        })
        .collect::<Vec<_>>();
    available_credits.sort_by_key(|credit| credit.expires_at.unwrap_or(i64::MAX));

    let mut options = available_credits
        .into_iter()
        .take(detail_limit)
        .map(|credit| {
            let expiration = match credit.expires_at {
                Some(expires_at) => DateTime::<Utc>::from_timestamp(expires_at, 0)
                    .map(|expires_at| {
                        let expires_at = expires_at.with_timezone(&Local);
                        format!(
                            "Expires {} on {}",
                            expires_at.format(clock_format.time_format()),
                            expires_at.format("%-d %b %Y")
                        )
                    })
                    .unwrap_or_else(|| "Expiration unavailable".to_string()),
                None => "Does not expire".to_string(),
            };
            let reset_title = credit
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .unwrap_or("Full reset");
            let reset_description = credit
                .description
                .as_deref()
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .unwrap_or("Reset your current usage limits");
            ResetCreditOption {
                credit_id: Some(credit.id.clone()),
                name: reset_title.to_string(),
                detail: Some(expiration),
                description: reset_description.to_string(),
            }
        })
        .collect::<Vec<_>>();

    if options.is_empty() && available_count > 0 && summary.credits.is_none() {
        options.push(ResetCreditOption {
            credit_id: None,
            name: "Full reset".to_string(),
            detail: None,
            description: "Reset your current usage limits".to_string(),
        });
    }

    options
}

impl ChatWidget {
    pub(crate) fn set_reset_credit_owner(&mut self, owner_key: Option<String>) {
        self.rate_limit_reset_owner_key = owner_key.filter(|owner| valid_reset_owner_key(owner));
    }

    pub(crate) fn reset_credit_owner_matches(&self, operation: &ResetCreditOperation) -> bool {
        self.rate_limit_reset_owner_key.as_deref() == Some(operation.owner_key.as_str())
    }

    pub(crate) fn set_pending_reset_credit_operation(
        &mut self,
        operation: Option<ResetCreditOperation>,
        persistence_error: Option<String>,
    ) {
        self.pending_rate_limit_reset_operation = operation;
        self.rate_limit_reset_persistence_error = persistence_error;
    }

    pub(crate) fn reset_credit_operation_can_start(
        &self,
        operation: &ResetCreditOperation,
    ) -> bool {
        self.rate_limit_reset_active_operation.is_none()
            && self.reset_credit_owner_matches(operation)
            && self.rate_limit_reset_persistence_error.is_none()
            && self
                .pending_rate_limit_reset_operation
                .as_ref()
                .or(self.rate_limit_reset_draft.as_ref())
                == Some(operation)
    }

    pub(crate) fn show_reset_credit_persistence_error(&mut self) {
        let message = self.rate_limit_reset_persistence_error.as_deref().unwrap_or(
            "Couldn't save this reset. No request was sent. Check Codex home permissions and try again.",
        );
        let params = Self::reset_refresh_params(message);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_CONFIRMATION_VIEW_ID);
        self.bottom_pane
            .dismiss_view_by_id(RATE_LIMIT_RESET_VIEW_ID);
        self.bottom_pane.show_selection_view(params);
        self.request_redraw();
    }

    pub(super) fn pending_reset_credit_params(&self) -> SelectionViewParams {
        if let Some(error) = &self.rate_limit_reset_persistence_error {
            return Self::reset_refresh_params(error);
        }
        let Some(operation) = self.pending_rate_limit_reset_operation.clone() else {
            return Self::reset_refresh_params("Reload usage before reviewing this reset.");
        };
        if !self.reset_credit_owner_matches(&operation) {
            return Self::rate_limit_reset_message_params(
                "Return to the original account to retry this reset. Its result is still unconfirmed.",
            );
        }
        SelectionViewParams {
            view_id: Some(RATE_LIMIT_RESET_VIEW_ID),
            title: Some("Usage limit resets".to_string()),
            subtitle: Some(
                "This reset's result is unconfirmed. Review it before using another reset."
                    .to_string(),
            ),
            footer_hint: Some(usage_hint_line(&self.bottom_pane.list_keymap(), "review")),
            items: vec![
                SelectionItem {
                    name: "Review original reset".to_string(),
                    description: Some("Retry the same reset safely".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ConsumeRateLimitResetCredit {
                            operation: operation.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Close".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        }
    }
}
