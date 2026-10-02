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

impl AccountIdentity {
    fn matches_auth(&self, auth: Option<&CodexAuth>) -> bool {
        auth.and_then(CodexAuth::get_account_id) == self.account_id
            && auth.and_then(CodexAuth::get_chatgpt_user_id) == self.user_id
            && auth.map(CodexAuth::api_auth_mode).map(auth_mode_to_api) == self.auth_mode
    }

    fn is_current(&self, pool: &ExecutionAccountPoolHandle, auth_manager: &AuthManager) -> bool {
        let owner_changes = auth_manager.auth_change_state_receiver();
        let owner_generation = owner_changes.borrow().owner_generation;
        let active = pool.active_identity();
        owner_generation == self.owner_generation
            && active.as_ref().map(|identity| identity.profile_id.as_str())
                == self.profile_id.as_deref()
            && active.as_ref().map(|identity| identity.generation) == self.generation
            && self.matches_auth(auth_manager.auth_cached().as_ref())
    }
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
        let (mut last_identity, mut last_quota, mut pool_was_enabled) =
            match read_state(&pool, &auth_manager, &config).await {
                Some((identity, account_pool, quota)) => {
                    (Some(identity), quota, account_pool.enabled)
                }
                None => (None, None, false),
            };
        loop {
            let changed = tokio::select! {
                changed = changes.changed() => changed,
                changed = auth_changes.changed() => changed,
            };
            if changed.is_err() {
                break;
            }
            let Some((identity, account_pool, quota)) =
                read_state(&pool, &auth_manager, &config).await
            else {
                // A later change remains pending on the receivers. Do not publish a
                // snapshot assembled across owners while rapid switching continues.
                continue;
            };
            if !account_pool.enabled && !pool_was_enabled {
                // Ordinary single-account login and routing notifications keep
                // their existing owner; this watcher only publishes pool transitions.
                last_identity = Some(identity);
                last_quota = None;
                continue;
            }
            pool_was_enabled = account_pool.enabled;
            if last_identity.as_ref() != Some(&identity) {
                outgoing
                    .send_server_notification_checked(
                        &[],
                        ServerNotification::AccountUpdated(AccountUpdatedNotification {
                            auth_mode: identity.auth_mode,
                            plan_type: identity.plan_type,
                            account_pool: Some(account_pool.clone()),
                        }),
                        || identity.is_current(&pool, &auth_manager),
                    )
                    .await;
            }
            *remote_client_registry.caption.lock().await =
                crate::mobile_account_status::pool_caption(&account_pool);
            if !identity.is_current(&pool, &auth_manager) {
                continue;
            }
            remote_client_registry
                .push_pool_update_checked(&outgoing, &account_pool, || {
                    identity.is_current(&pool, &auth_manager)
                })
                .await;
            if !identity.is_current(&pool, &auth_manager) {
                continue;
            }
            if quota != last_quota {
                if let Some(rate_limits) = &quota {
                    // This source is the verified active profile's merged cache. Broadcasting
                    // the stable quota event also updates mobile clients that ignore pool RPCs;
                    // untagged inference events remain suppressed on their targeted connections.
                    outgoing
                        .send_server_notification_checked(
                            &[],
                            ServerNotification::AccountRateLimitsUpdated(
                                codex_app_server_protocol::AccountRateLimitsUpdatedNotification {
                                    rate_limits: rate_limits.clone(),
                                },
                            ),
                            || identity.is_current(&pool, &auth_manager),
                        )
                        .await;
                }
                last_quota = quota;
            }
            if !identity.is_current(&pool, &auth_manager) {
                continue;
            }
            let notification = codex_app_server_protocol::AccountPoolUpdatedNotification {
                active_profile_id: account_pool.active_profile_id,
                active_generation: account_pool.active_generation,
                accounts: account_pool.accounts,
            };
            outgoing
                .send_server_notification_checked(
                    &[],
                    ServerNotification::AccountPoolUpdated(notification),
                    || identity.is_current(&pool, &auth_manager),
                )
                .await;
            last_identity = Some(identity);
        }
    })
}

async fn read_state(
    pool: &ExecutionAccountPoolHandle,
    auth_manager: &AuthManager,
    config: &Config,
) -> Option<(
    AccountIdentity,
    codex_app_server_protocol::AccountPoolReadResponse,
    Option<codex_app_server_protocol::RateLimitSnapshot>,
)> {
    let auth_changes = auth_manager.auth_change_state_receiver();
    for _ in 0..3 {
        let auth_state = *auth_changes.borrow();
        let active = pool.active_identity();
        let profile_manager = active.as_ref().and_then(|active| {
            pool.auth_managers()
                .into_iter()
                .find_map(|(id, manager)| (id == active.profile_id).then_some(manager))
        });
        if active.is_some() && profile_manager.is_none() {
            continue;
        }
        let profile_state = profile_manager
            .as_ref()
            .map(|manager| *manager.auth_change_state_receiver().borrow());
        let account_pool = build_account_pool_read_response(config, pool).await;
        let auth = auth_manager.auth().await;
        let identity = AccountIdentity {
            profile_id: account_pool.active_profile_id.clone(),
            generation: account_pool.active_generation,
            owner_generation: auth_state.owner_generation,
            auth_mode: auth
                .as_ref()
                .map(CodexAuth::api_auth_mode)
                .map(auth_mode_to_api),
            plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
            account_id: auth.as_ref().and_then(CodexAuth::get_account_id),
            user_id: auth.as_ref().and_then(CodexAuth::get_chatgpt_user_id),
        };
        if let Some(manager) = &profile_manager {
            let current_profile_state = *manager.auth_change_state_receiver().borrow();
            if !identity.matches_auth(manager.auth_cached().as_ref())
                || Some(current_profile_state) != profile_state
                || !account_pool.accounts.iter().any(|account| {
                    account.is_active
                        && Some(account.profile_id.as_str()) == identity.profile_id.as_deref()
                })
            {
                continue;
            }
        }
        let quota = quota_snapshot(pool, &identity, &account_pool);
        let current_auth_state = *auth_changes.borrow();
        let current_profile_state = profile_manager
            .as_ref()
            .map(|manager| *manager.auth_change_state_receiver().borrow());
        if current_auth_state == auth_state
            && current_profile_state == profile_state
            && pool.active_identity() == active
            && active.as_ref().map(|identity| identity.profile_id.as_str())
                == identity.profile_id.as_deref()
            && active.as_ref().map(|identity| identity.generation) == identity.generation
        {
            return Some((identity, account_pool, quota));
        }
    }
    None
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
    let profile_owner_generation = manager
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    let auth = manager.auth_cached()?;
    if !identity.matches_auth(Some(&auth)) {
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
