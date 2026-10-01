//! Quota probes and window-wise evidence for standby window warmup.

use super::*;

pub(crate) fn prefer_rate_limit_snapshot(
    current: Option<RateLimitSnapshot>,
    incoming: RateLimitSnapshot,
) -> RateLimitSnapshot {
    let Some(current) = current else {
        return incoming;
    };
    let incoming_is_codex = incoming
        .limit_id
        .as_deref()
        .is_none_or(|limit_id| limit_id == "codex");
    let current_is_codex = current
        .limit_id
        .as_deref()
        .is_none_or(|limit_id| limit_id == "codex");
    match (current_is_codex, incoming_is_codex) {
        (false, true) => incoming,
        (true, false) => current,
        _ => {
            let current_used = current
                .primary
                .as_ref()
                .map(|window| window.used_percent)
                .unwrap_or(0.0);
            let incoming_used = incoming
                .primary
                .as_ref()
                .map(|window| window.used_percent)
                .unwrap_or(0.0);
            if incoming_used > current_used {
                incoming
            } else {
                current
            }
        }
    }
}

pub(super) fn account_primary_started(limits: &AccountRateLimits) -> bool {
    limits.primary.as_ref().is_some_and(|window| {
        window.used_percent > 0.0
            && window.window_minutes.is_none_or(|minutes| minutes == 300)
            && window.resets_at.is_none_or(|reset| reset > Utc::now())
    })
}

pub(super) fn merge_account_rate_limits_monotonic(
    existing: Option<&AccountRateLimits>,
    incoming: AccountRateLimits,
) -> AccountRateLimits {
    let Some(existing) = existing else {
        return incoming;
    };
    let mut merged = incoming;
    if let Some(existing_primary) = existing.primary.as_ref()
        && existing_primary.used_percent > 0.0
    {
        let regresses = merged
            .primary
            .as_ref()
            .is_none_or(|window| window.used_percent <= 0.0);
        let reset_due = existing_primary
            .resets_at
            .is_some_and(|resets_at| resets_at <= Utc::now());
        let new_window = merged.primary.as_ref().is_some_and(|window| {
            matches!((existing_primary.resets_at, window.resets_at),
                (Some(previous), Some(incoming)) if incoming > previous)
        });
        if regresses && !reset_due && !new_window {
            merged.primary = Some(existing_primary.clone());
        }
    }
    if merged.secondary.is_none() {
        merged.secondary = existing.secondary.clone();
    }
    if merged.observed_at.is_none() {
        merged.observed_at = existing.observed_at;
    }
    merged
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
