//! Normalizes and bounds authenticated reset-credit metadata for display and notices.

use super::ExpiringResetCredit;
use super::MAX_CREDITS;
use crate::RateLimitResetCreditsDetails;
use chrono::DateTime;

pub(super) fn filter_credits(
    details: RateLimitResetCreditsDetails,
    now: i64,
) -> Vec<ExpiringResetCredit> {
    let mut credits = Vec::new();
    if details.available_count <= 0 {
        return credits;
    }
    for credit in details.credits {
        if credit.reset_type != "codex_rate_limits"
            || credit.status != "available"
            || credit.id.trim().is_empty()
            || credit.id.len() > 256
            || credit.id.chars().any(char::is_control)
        {
            continue;
        }
        let Some(expiry) = credit
            .expires_at
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        else {
            continue;
        };
        let expires_at = expiry.timestamp();
        if expires_at <= now
            || credits
                .iter()
                .any(|value: &ExpiringResetCredit| value.id == credit.id)
        {
            continue;
        }
        credits.push(ExpiringResetCredit {
            id: credit.id,
            expires_at,
            title: credit
                .title
                .map(|value| bounded_text(&value, /*max_chars*/ 160))
                .filter(|value| !value.is_empty()),
            description: credit
                .description
                .map(|value| bounded_text(&value, /*max_chars*/ 320))
                .filter(|value| !value.is_empty()),
        });
        credits.sort_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        credits.truncate(MAX_CREDITS);
    }
    credits
}

pub(super) fn bounded_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .take(max_chars)
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_string()
}
