//! Separate execution-identity updates from routine quota and availability observations.

use super::*;

#[derive(Clone, PartialEq, Eq)]
struct AccountIdentity {
    profile_id: Option<String>,
    generation: Option<u64>,
    owner_generation: u64,
    auth_mode: Option<codex_app_server_protocol::AuthMode>,
    plan_type: Option<codex_protocol::account::PlanType>,
    account_id: Option<String>,
    user_id: Option<String>,
}

pub(super) fn spawn(
    pool: ExecutionAccountPoolHandle,
    auth_manager: Arc<AuthManager>,
    config: Arc<Config>,
    outgoing: Arc<OutgoingMessageSender>,
    remote_client_registry: Arc<RemoteClientRegistry>,
) -> tokio::task::JoinHandle<()> {
    let mut changes = pool.change_receiver();
    let mut auth_changes = auth_manager.auth_change_state_receiver();
    tokio::spawn(async move {
        let (mut last_identity, initial_pool) = read_state(&pool, &auth_manager, &config).await;
        let mut last_quota = quota_snapshot(&pool, &last_identity, &initial_pool);
        let mut pool_was_enabled = initial_pool.enabled;
        loop {
            let changed = tokio::select! {
                changed = changes.changed() => changed,
                changed = auth_changes.changed() => changed,
            };
            if changed.is_err() {
                break;
            }
            let (identity, account_pool) = read_state(&pool, &auth_manager, &config).await;
            if !account_pool.enabled && !pool_was_enabled {
                // Ordinary single-account login and routing notifications keep
                // their existing owner; this watcher only publishes pool transitions.
                last_identity = identity;
                last_quota = None;
                continue;
            }
            pool_was_enabled = account_pool.enabled;
            if identity != last_identity {
                outgoing
                    .send_server_notification(ServerNotification::AccountUpdated(
                        AccountUpdatedNotification {
                            auth_mode: identity.auth_mode,
                            plan_type: identity.plan_type,
                            account_pool: Some(account_pool.clone()),
                        },
                    ))
                    .await;
            }
            last_identity = identity;
            *remote_client_registry.caption.lock().await =
                crate::mobile_account_status::pool_caption(&account_pool);
            let quota = quota_snapshot(&pool, &last_identity, &account_pool);
            if quota != last_quota {
                if let Some(rate_limits) = &quota {
                    // This source is the verified active profile's merged cache. Broadcasting
                    // the stable quota event also updates mobile clients that ignore pool RPCs;
                    // untagged inference events remain suppressed on their targeted connections.
                    outgoing
                        .send_server_notification(ServerNotification::AccountRateLimitsUpdated(
                            codex_app_server_protocol::AccountRateLimitsUpdatedNotification {
                                rate_limits: rate_limits.clone(),
                            },
                        ))
                        .await;
                }
                last_quota = quota;
            }
            let notification = codex_app_server_protocol::AccountPoolUpdatedNotification {
                active_profile_id: account_pool.active_profile_id,
                active_generation: account_pool.active_generation,
                accounts: account_pool.accounts,
            };
            outgoing
                .send_server_notification(ServerNotification::AccountPoolUpdated(notification))
                .await;
        }
    })
}

async fn read_state(
    pool: &ExecutionAccountPoolHandle,
    auth_manager: &AuthManager,
    config: &Config,
) -> (
    AccountIdentity,
    codex_app_server_protocol::AccountPoolReadResponse,
) {
    let account_pool = build_account_pool_read_response(config, pool).await;
    let auth = auth_manager.auth().await;
    let identity = AccountIdentity {
        profile_id: account_pool.active_profile_id.clone(),
        generation: account_pool.active_generation,
        owner_generation: auth_manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation,
        auth_mode: auth
            .as_ref()
            .map(CodexAuth::api_auth_mode)
            .map(auth_mode_to_api),
        plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
        account_id: auth.as_ref().and_then(CodexAuth::get_account_id),
        user_id: auth.as_ref().and_then(CodexAuth::get_chatgpt_user_id),
    };
    (identity, account_pool)
}

fn quota_snapshot(
    pool: &ExecutionAccountPoolHandle,
    identity: &AccountIdentity,
    response: &codex_app_server_protocol::AccountPoolReadResponse,
) -> Option<codex_app_server_protocol::RateLimitSnapshot> {
    let active = pool.active_identity()?;
    if Some(active.profile_id.as_str()) != identity.profile_id.as_deref()
        || Some(active.generation) != identity.generation
    {
        return None;
    }
    let manager = pool
        .auth_managers()
        .into_iter()
        .find(|(id, _)| *id == active.profile_id)?
        .1;
    let auth = manager.auth_cached()?;
    let profile_owner_generation = manager
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    if auth.get_account_id() != identity.account_id
        || auth.get_chatgpt_user_id() != identity.user_id
        || Some(auth_mode_to_api(auth.api_auth_mode())) != identity.auth_mode
    {
        return None;
    }
    let account = pool
        .snapshots()
        .into_iter()
        .find(|account| account.is_active && account.profile.id == active.profile_id)?;
    if pool.active_identity().as_ref() != Some(&active)
        || manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            != profile_owner_generation
    {
        return None;
    }
    let window =
        |window: codex_login::AccountRateLimitWindow| codex_app_server_protocol::RateLimitWindow {
            used_percent: window.used_percent.clamp(0.0, 100.0).round() as i32,
            window_duration_mins: window.window_minutes,
            resets_at: window.resets_at.map(|time| time.timestamp()),
        };
    let primary = account
        .rate_limits
        .primary
        .filter(|window| window.used_percent.is_finite())
        .map(window);
    let secondary = account
        .rate_limits
        .secondary
        .filter(|window| window.used_percent.is_finite())
        .map(window);
    if primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(codex_app_server_protocol::RateLimitSnapshot {
        limit_id: Some("codex".into()),
        limit_name: crate::mobile_account_status::quota_caption(response),
        normal_model_slug: None,
        primary,
        secondary,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: identity.plan_type,
        rate_limit_reached_type: None,
    })
}
