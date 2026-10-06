//! Refresh receipts describe accepted windows separately from backend usage permission.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum QuotaRefreshOutcome {
    Updated,
    Incomplete,
    Denied,
    Failed,
}

pub(super) struct QuotaRefreshReport {
    pub(super) outcome: QuotaRefreshOutcome,
    pub(super) message: String,
}

pub(super) fn quota_refresh_report(
    response: &codex_backend_client::RateLimitsWithResetCredits,
    saved: &codex_login::AccountRuntimeProfileState,
    requested_at: DateTime<Utc>,
) -> QuotaRefreshReport {
    let snapshot = response
        .rate_limits
        .iter()
        .find(|snapshot| snapshot.limit_id.as_deref().is_none_or(|id| id == "codex"));
    let restricted = snapshot.is_some_and(|snapshot| {
        snapshot.spend_control_reached == Some(true)
            || snapshot
                .individual_limit
                .as_ref()
                .is_some_and(|limit| limit.remaining_percent <= 0)
            || snapshot.rate_limit_reached_type.is_some()
    });
    if response.ordinary_usage_allowed == Some(false) || restricted {
        return QuotaRefreshReport {
            outcome: QuotaRefreshOutcome::Denied,
            message: if response.ordinary_usage_allowed == Some(false) {
                "Backend denies ordinary usage; cached percentages do not grant permission. Wait for reset or inspect available reset credits."
            } else {
                "Backend reports a quota or workspace restriction. Inspect account access or reset credits before retrying."
            }.into(),
        };
    }
    let windows = [
        (
            "Primary",
            snapshot.and_then(|snapshot| snapshot.primary.as_ref()),
            saved.rate_limits.primary.as_ref(),
            saved.rate_limits.primary_observed_at(),
        ),
        (
            "Secondary",
            snapshot.and_then(|snapshot| snapshot.secondary.as_ref()),
            saved.rate_limits.secondary.as_ref(),
            saved.rate_limits.secondary_observed_at(),
        ),
    ];
    let mut incomplete = false;
    let mut details = Vec::new();
    for (name, returned, retained, observed_at) in windows {
        let detail = if returned.is_none() {
            incomplete = true;
            format!("{name} window not refreshed (omitted by backend)")
        } else if observed_at.is_none_or(|at| at < requested_at) {
            incomplete = true;
            format!("{name} window not refreshed (older or conflicting response)")
        } else if retained
            .is_none_or(|window| window.resets_at.is_none_or(|reset| reset <= requested_at))
        {
            incomplete = true;
            format!("{name} window remains stale or incomplete")
        } else {
            format!("{name} window updated")
        };
        details.push(detail);
    }
    if incomplete {
        return QuotaRefreshReport {
            outcome: QuotaRefreshOutcome::Incomplete,
            message: format!("{}. Refresh again; cached values kept.", details.join("; ")),
        };
    }
    QuotaRefreshReport {
        outcome: QuotaRefreshOutcome::Updated,
        message: if saved.exhausted_until.is_some_and(|until| until > requested_at) {
            "Both quota windows updated; local recovery remains unconfirmed. Refresh again, or retry after an external reset."
        } else {
            "Quota updated"
        }.into(),
    }
}

// Owns a refresh reservation even while it waits for the concurrency budget.
pub(super) struct RefreshPermit {
    pub(super) statuses: Arc<std::sync::Mutex<HashMap<(String, String), RefreshStatus>>>,
    pub(super) key: (String, String),
    pub(super) completed: bool,
}

impl Drop for RefreshPermit {
    fn drop(&mut self) {
        if !self.completed
            && let Some(status) = self
                .statuses
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&self.key)
        {
            status.in_progress = false;
            status.message = "Quota check interrupted; refresh to try again".into();
        }
    }
}
