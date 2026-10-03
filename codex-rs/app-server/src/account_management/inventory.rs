use super::*;
use chrono::Utc;
use codex_login::AccountProfileState;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;

impl AccountManager {
    /// Lists all enrolled profiles without activating accounts or refreshing OAuth credentials.
    pub async fn inventory(&self) -> anyhow::Result<AccountManagerInventory> {
        let store = self.store();
        let state = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf()).load()?;
        let paused = codex_login::AccountPoolRuntime::is_home_suspended(&self.config.codex_home);
        let refreshes = self
            .refreshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut accounts = Vec::new();
        for record in store.load_profile_records()? {
            let saved = state
                .profiles
                .iter()
                .find(|saved| saved.profile_id == record.profile.id);
            let mut auth_config = self.config.auth_config();
            auth_config.codex_home = record.profile.credential_home.clone();
            let auth = if record.state == AccountProfileState::PendingLogin {
                None
            } else {
                AuthManager::shared_from_auth_config(
                    auth_config,
                    /*enable_codex_api_key_env*/ false,
                )
                .await
                .ok()
                .and_then(|manager| manager.auth_cached())
            };
            let email = auth
                .as_ref()
                .and_then(codex_login::CodexAuth::get_account_email);
            let cooldown = saved
                .and_then(|saved| saved.exhausted_until)
                .filter(|until| *until > Utc::now());
            let login_state = match record.state {
                AccountProfileState::PendingLogin => "pending",
                AccountProfileState::Ready
                    if auth
                        .as_ref()
                        .is_some_and(codex_login::CodexAuth::is_chatgpt_auth) =>
                {
                    "signedIn"
                }
                AccountProfileState::Ready => "needsLogin",
            };
            let availability = if record.profile.disabled {
                "disabled"
            } else if login_state != "signedIn" {
                "needsLogin"
            } else if cooldown.is_some() {
                "coolingDown"
            } else if paused {
                "paused"
            } else {
                "ready"
            };
            let limits = saved
                .map(|saved| saved.rate_limits.clone())
                .unwrap_or_default();
            let window = |window: &codex_login::AccountRateLimitWindow| ManagedRateLimitWindow {
                used_percent: window.used_percent,
                resets_at: window.resets_at.map(|at| at.timestamp()),
                window_minutes: window.window_minutes,
            };
            let refresh = refreshes.get(record.profile.id.as_str()).cloned();
            accounts.push(ManagedAccountView {
                profile_id: record.profile.id.to_string(),
                label: codex_login::account_display::account_display_name(
                    record.profile.label.as_deref(),
                    email.as_deref(),
                    record.profile.id.as_str(),
                )
                .to_string(),
                custom_label: record.profile.label.clone(),
                priority: record.profile.priority,
                disabled: record.profile.disabled,
                login_state: login_state.into(),
                availability: availability.into(),
                cooldown_until: cooldown.map(|at| at.timestamp()),
                backend_resets_at: saved
                    .and_then(|saved| saved.backend_resets_at)
                    .map(|at| at.timestamp()),
                plan: auth
                    .as_ref()
                    .and_then(codex_login::CodexAuth::account_plan_type)
                    .map(|plan| {
                        let value = serde_json::to_value(plan).unwrap_or_default();
                        serde_json::from_value::<codex_protocol::auth::KnownPlan>(value)
                            .map_or_else(|_| "Unknown".into(), |plan| plan.display_name().into())
                    }),
                email,
                rate_limits: ManagedRateLimits {
                    primary: limits.primary.as_ref().map(window),
                    secondary: limits.secondary.as_ref().map(window),
                    observed_at: limits.observed_at.map(|at| at.timestamp()),
                    primary_observed_at: limits.primary_observed_at().map(|at| at.timestamp()),
                    secondary_observed_at: limits.secondary_observed_at().map(|at| at.timestamp()),
                },
                reset_credit_count: refresh
                    .as_ref()
                    .and_then(|refresh| refresh.reset_credit_count),
                refresh,
            });
        }
        accounts.sort_by_key(|account| (account.priority, account.profile_id.clone()));
        let login_jobs = self.login_progress().await;
        let config = codex_core::config::ConfigBuilder::default()
            .codex_home(self.config.codex_home.to_path_buf())
            .build()
            .await?;
        let (api_accounts, api_state) = self.api_inventory()?;
        Ok(AccountManagerInventory {
            host_now: Utc::now().timestamp(),
            primary_login: Some(self.primary_login_view()),
            paused,
            active_profile_id: (!paused)
                .then_some(state.active_profile_id)
                .flatten()
                .map(|id| id.to_string()),
            accounts,
            settings: serde_json::to_value(config.account_pool)?,
            login_jobs,
            api_accounts,
            api_selection: api_state.selection,
            api_fallback: api_state.fallback,
        })
    }
}
