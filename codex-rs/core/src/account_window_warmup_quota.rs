//! Quota probes and window-wise evidence for standby window warmup.

use super::*;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;

pub(crate) fn prefer_rate_limit_snapshot(
    current: Option<RateLimitSnapshot>,
    mut incoming: RateLimitSnapshot,
) -> RateLimitSnapshot {
    let Some(current) = current else {
        return incoming;
    };
    let is_codex =
        |snapshot: &RateLimitSnapshot| snapshot.limit_id.as_deref().is_none_or(|id| id == "codex");
    match (is_codex(&current), is_codex(&incoming)) {
        (false, true) => return incoming,
        (true, false) => return current,
        (false, false) | (true, true) => {}
    }
    let merged = merge_account_rate_limits_monotonic(
        Some(&convert_rate_limits(&current)),
        convert_rate_limits(&incoming),
    );
    let convert = |window: AccountRateLimitWindow| RateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window.resets_at.map(|reset| reset.timestamp()),
        window_minutes: window.window_minutes,
    };
    incoming.primary = merged.primary.map(convert);
    incoming.secondary = merged.secondary.map(convert);
    incoming.limit_id = incoming.limit_id.or(current.limit_id);
    incoming.credits = incoming.credits.or(current.credits);
    incoming.rate_limit_reached_type = incoming
        .rate_limit_reached_type
        .or(current.rate_limit_reached_type);
    incoming
}

pub(super) fn account_primary_started(limits: &AccountRateLimits) -> bool {
    let now = Utc::now();
    limits.primary.as_ref().is_some_and(|window| {
        window.used_percent.is_finite()
            && window.used_percent > 0.0
            && window.window_minutes.is_none_or(|minutes| minutes == 300)
            && match window.resets_at {
                Some(reset) => reset > now && reset <= now + chrono::Duration::minutes(305),
                None => limits.observed_at.is_some_and(|observed| {
                    observed <= now && now - observed < chrono::Duration::minutes(30)
                }),
            }
    })
}

pub(super) fn quota_allows_generation(limits: &AccountRateLimits) -> bool {
    let Some(primary) = limits.primary.as_ref() else {
        return false;
    };
    primary.used_percent.is_finite()
        && primary.used_percent == 0.0
        && primary.window_minutes == Some(300)
        && primary
            .resets_at
            .is_none_or(|reset| reset <= Utc::now() + chrono::Duration::minutes(305))
        && limits.secondary.as_ref().is_none_or(|secondary| {
            secondary.used_percent.is_finite()
                && secondary.used_percent >= 0.0
                && secondary.used_percent < 100.0
        })
}

pub(super) fn same_warmup_identity(expected: &CodexAuth, current: &CodexAuth) -> bool {
    expected.is_chatgpt_auth()
        && current.is_chatgpt_auth()
        && expected.get_account_id().is_some()
        && expected.get_chatgpt_user_id().is_some()
        && expected.get_account_id() == current.get_account_id()
        && expected.get_chatgpt_user_id() == current.get_chatgpt_user_id()
}

pub(super) fn request_was_definitely_rejected(error: &anyhow::Error) -> bool {
    error.downcast_ref::<CodexErr>().is_some_and(|error| {
        error
            .http_status_code_value()
            .is_some_and(|status| (400..500).contains(&status) && status != 408)
            || matches!(
                error.details(),
                CodexErrorDetails::InvalidRequest(_)
                    | CodexErrorDetails::InvalidPrompt { .. }
                    | CodexErrorDetails::UsageLimitReached(_)
                    | CodexErrorDetails::QuotaExceeded
                    | CodexErrorDetails::UsageNotIncluded
                    | CodexErrorDetails::ContextWindowExceeded
                    | CodexErrorDetails::FlexUnavailable
                    | CodexErrorDetails::ServerOverloaded
            )
    })
}

pub(super) fn merge_account_rate_limits_monotonic(
    existing: Option<&AccountRateLimits>,
    incoming: AccountRateLimits,
) -> AccountRateLimits {
    let Some(existing) = existing else {
        return incoming;
    };
    if matches!((existing.observed_at, incoming.observed_at), (Some(previous), Some(current)) if current < previous)
    {
        return existing.clone();
    }
    AccountRateLimits {
        primary: merge_window(existing.primary.as_ref(), incoming.primary),
        secondary: merge_window(existing.secondary.as_ref(), incoming.secondary),
        observed_at: incoming.observed_at.or(existing.observed_at),
    }
}

fn merge_window(
    existing: Option<&AccountRateLimitWindow>,
    incoming: Option<AccountRateLimitWindow>,
) -> Option<AccountRateLimitWindow> {
    let Some(existing) = existing else {
        return incoming;
    };
    let Some(mut incoming) = incoming else {
        return Some(existing.clone());
    };
    if !incoming.used_percent.is_finite() || incoming.used_percent < 0.0 {
        return Some(existing.clone());
    }
    if matches!((existing.window_minutes, incoming.window_minutes), (Some(previous), Some(current)) if previous != current)
    {
        return Some(existing.clone());
    }
    let now = Utc::now();
    if existing.used_percent <= 0.0
        && incoming.used_percent.is_finite()
        && incoming.used_percent > 0.0
        && existing.window_minutes.is_none_or(|minutes| minutes == 300)
        && incoming.window_minutes.is_none_or(|minutes| minutes == 300)
        && matches!((existing.resets_at, incoming.resets_at),
            (Some(previous), Some(current)) if current > now
                && current <= now + chrono::Duration::minutes(305)
                && previous <= now + chrono::Duration::minutes(305)
                && previous - current <= chrono::Duration::minutes(5))
    {
        return Some(incoming);
    }
    match (existing.resets_at, incoming.resets_at) {
        (Some(previous), Some(current)) if current < previous => return Some(existing.clone()),
        (Some(previous), Some(current)) if current > previous => return Some(incoming),
        (Some(previous), _) if previous <= Utc::now() => return Some(incoming),
        (Some(_), Some(_)) | (Some(_), None) | (None, Some(_)) | (None, None) => {}
    }
    incoming.used_percent = incoming.used_percent.max(existing.used_percent);
    incoming.resets_at = incoming.resets_at.or(existing.resets_at);
    incoming.window_minutes = incoming.window_minutes.or(existing.window_minutes);
    Some(incoming)
}

pub(super) async fn refresh_rate_limits_via_get_with_retries(
    config: &Config,
    auth: &CodexAuth,
    stream_started: bool,
) -> Option<AccountRateLimits> {
    let mut best = None;
    for (index, delay_secs) in GET_VERIFY_DELAYS_SECS.iter().enumerate() {
        if *delay_secs > 0 {
            tokio::time::sleep(Duration::from_secs(*delay_secs)).await;
        }
        let Some(limits) = refresh_rate_limits_via_get(config, auth).await else {
            continue;
        };
        if account_primary_started(&limits) {
            return Some(limits);
        }
        // Keep the freshest idle snapshot; if Responses already proved a start, preserve that
        // below via monotonic merge with stream limits.
        best = Some(limits);
        // After a stream-proven start, one confirming GET is enough.
        if stream_started && index == 0 {
            break;
        }
    }
    best
}

pub(super) fn primary_window_started(pool: &AccountPool, profile_id: &AccountProfileId) -> bool {
    pool.snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .is_some_and(|snapshot| account_primary_started(&snapshot.rate_limits))
}

pub(super) async fn refresh_rate_limits_via_get(
    config: &Config,
    auth: &CodexAuth,
) -> Option<AccountRateLimits> {
    let client = BackendClient::from_auth(
        config.chatgpt_base_url.clone(),
        auth,
        config.http_client_factory(),
    );
    let observed_at = Utc::now();
    let snapshots = tokio::time::timeout(GET_REFRESH_TIMEOUT, client.get_rate_limits_many())
        .await
        .ok()?
        .ok()?;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| {
            snapshots
                .iter()
                .find(|snapshot| snapshot.limit_id.is_none())
        })?;
    Some(AccountRateLimits {
        primary: snapshot.primary.as_ref().map(convert_rate_limit_window),
        secondary: snapshot.secondary.as_ref().map(convert_rate_limit_window),
        observed_at: Some(observed_at),
    })
}

pub(super) fn convert_rate_limits(snapshot: &RateLimitSnapshot) -> AccountRateLimits {
    AccountRateLimits {
        primary: snapshot.primary.as_ref().map(convert_rate_limit_window),
        secondary: snapshot.secondary.as_ref().map(convert_rate_limit_window),
        observed_at: Some(Utc::now()),
    }
}

fn convert_rate_limit_window(window: &RateLimitWindow) -> AccountRateLimitWindow {
    AccountRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window
            .resets_at
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
        window_minutes: window.window_minutes,
    }
}
