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
                None => limits.primary_observed_at().is_some_and(|observed| {
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
    match existing {
        Some(existing) => codex_login::merge_rate_limits_monotonic(existing, incoming),
        None => incoming,
    }
}

pub(super) struct WarmupQuotaProbe {
    pub(super) limits: AccountRateLimits,
    pub(super) ordinary_allowed: bool,
}

pub(super) async fn refresh_rate_limits_via_get_with_retries(
    config: &Config,
    auth_manager: &AuthManager,
    auth: &CodexAuth,
    stream_started: bool,
) -> anyhow::Result<Option<AccountRateLimits>> {
    let mut best = None;
    for (index, delay_secs) in GET_VERIFY_DELAYS_SECS.iter().enumerate() {
        if *delay_secs > 0 {
            tokio::time::sleep(Duration::from_secs(*delay_secs)).await;
        }
        let Some(limits) = refresh_rate_limits_via_get(config, auth_manager, auth).await? else {
            if stream_started {
                break;
            }
            continue;
        };
        let limits = merge_account_rate_limits_monotonic(best.as_ref(), limits.limits);
        if account_primary_started(&limits) {
            return Ok(Some(limits));
        }
        best = Some(limits);
        if stream_started && index == 0 {
            break;
        }
    }
    Ok(best)
}

pub(super) async fn refresh_rate_limits_via_get(
    config: &Config,
    auth_manager: &AuthManager,
    expected_auth: &CodexAuth,
) -> anyhow::Result<Option<WarmupQuotaProbe>> {
    auth_manager.reload().await;
    let Some((auth, _)) = auth_manager.auth_with_http_client_factory().await else {
        anyhow::bail!("standby credentials became unavailable during warmup");
    };
    if !same_warmup_identity(expected_auth, &auth) {
        anyhow::bail!("standby user or workspace changed during warmup");
    }
    let client = BackendClient::from_auth(
        config.chatgpt_base_url.clone(),
        &auth,
        config.http_client_factory(),
    );
    let observed_at = Utc::now();
    let observed = match tokio::time::timeout(
        GET_REFRESH_TIMEOUT,
        client.get_rate_limits_with_reset_credits(),
    )
    .await
    {
        Ok(Ok(snapshots)) => snapshots,
        Ok(Err(_)) | Err(_) => return Ok(None),
    };
    // Re-login may have replaced the seat while the GET was in flight. Its quota must not be
    // published under the prior profile identity, even if the HTTP request itself succeeded.
    auth_manager.reload().await;
    if !auth_manager
        .auth_cached()
        .as_ref()
        .is_some_and(|current| same_warmup_identity(expected_auth, current))
    {
        anyhow::bail!("standby user or workspace changed while refreshing quota");
    }
    if observed
        .account_id
        .as_ref()
        .is_some_and(|id| Some(id) != auth.get_account_id().as_ref())
        || observed
            .user_id
            .as_ref()
            .is_some_and(|id| Some(id) != auth.get_chatgpt_user_id().as_ref())
    {
        anyhow::bail!("standby quota belongs to a different user or workspace");
    }
    let snapshot = observed
        .rate_limits
        .iter()
        .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| {
            observed
                .rate_limits
                .iter()
                .find(|snapshot| snapshot.limit_id.is_none())
        });
    Ok(snapshot.map(|snapshot| WarmupQuotaProbe {
        ordinary_allowed: observed.ordinary_usage_allowed != Some(false)
            && snapshot.spend_control_reached != Some(true),
        limits: AccountRateLimits {
            observed_at: Some(observed_at),
            ..convert_rate_limits(snapshot)
        },
    }))
}

pub(super) fn convert_rate_limits(snapshot: &RateLimitSnapshot) -> AccountRateLimits {
    let convert = |window: &RateLimitWindow| AccountRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window
            .resets_at
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
        window_minutes: window.window_minutes,
    };
    AccountRateLimits {
        primary: snapshot.primary.as_ref().map(convert),
        secondary: snapshot.secondary.as_ref().map(convert),
        observed_at: Some(Utc::now()),

        window_observed_at: None,
    }
}
