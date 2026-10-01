use base64::Engine as _;
use chrono::Utc;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::config::ConfigBuilder;

#[test]
fn only_managed_chatgpt_on_the_builtin_openai_provider_is_pool_eligible() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let openai = providers["openai"].clone();
    let ollama = providers["ollama"].clone();
    let bedrock = providers["amazon-bedrock"].clone();
    let mut custom_bearer = openai.clone();
    custom_bearer.name = "custom".to_string();
    custom_bearer.requires_openai_auth = false;
    custom_bearer.experimental_bearer_token = Some("custom-token".into());

    let cases = [
        (
            "managed ChatGPT",
            "openai",
            &openai,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Eligible,
        ),
        (
            "managed-only pool before profile resolution",
            "openai",
            &openai,
            None,
            false,
            PoolEligibility::Eligible,
        ),
        (
            "API key",
            "openai",
            &openai,
            Some(AuthMode::ApiKey),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "external ChatGPT tokens",
            "openai",
            &openai,
            Some(AuthMode::ChatgptAuthTokens),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "local provider",
            "ollama",
            &ollama,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "Bedrock",
            "amazon-bedrock",
            &bedrock,
            Some(AuthMode::BedrockApiKey),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "custom bearer",
            "custom",
            &custom_bearer,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "workload identity",
            "openai",
            &openai,
            Some(AuthMode::Chatgpt),
            true,
            PoolEligibility::Ineligible,
        ),
    ];

    for (name, provider_id, provider, auth_mode, workload_identity_selected, expected) in cases {
        assert_eq!(
            pool_eligibility(provider_id, provider, auth_mode, workload_identity_selected,),
            expected,
            "unexpected eligibility for {name}",
        );
    }
}

#[tokio::test]
async fn non_openai_provider_ignores_configured_account_pool() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("account-profiles.json"),
        "invalid manifest",
    )?;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    config.model_provider_id = "ollama".to_string();
    config.model_provider = built_in_model_providers(/*openai_base_url*/ None)["ollama"].clone();
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("test"),
    ));

    let enabled = execution_auth.ensure_runtime_from_config(&config).await?;

    assert!(!enabled);
    assert!(execution_auth.runtime().is_none());
    Ok(())
}

#[tokio::test]
async fn startup_prewarm_is_skipped_only_for_an_eligible_configured_pool() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));

    assert!(!execution_auth.should_skip_startup_prewarm(&config, &config.model_provider));

    std::fs::write(
        codex_home.path().join("account-profiles.json"),
        "configured",
    )?;
    assert!(execution_auth.should_skip_startup_prewarm(&config, &config.model_provider));

    let ollama = &built_in_model_providers(/*openai_base_url*/ None)["ollama"];
    assert!(!execution_auth.should_skip_startup_prewarm(&config, ollama));
    Ok(())
}

#[tokio::test]
async fn ensure_from_config_registers_profiles_added_after_install() -> anyhow::Result<()> {
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::AccountProfileStore;
    use codex_login::AuthDotJson;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::TokenData;
    use codex_login::save_auth;
    use codex_login::token_data::parse_chatgpt_jwt_claims;

    fn chatgpt_auth(account_id: &str) -> AuthDotJson {
        let header = serde_json::json!({"alg": "none", "typ": "JWT"});
        let payload = serde_json::json!({
            "email": format!("{account_id}@example.com"),
            "https://api.openai.com/auth": {
                "chatgpt_user_id": account_id,
                "user_id": account_id,
                "chatgpt_account_id": account_id
            }
        });
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let fake_jwt = format!(
            "{}.{}.sig",
            b64(&serde_json::to_vec(&header).expect("header")),
            b64(&serde_json::to_vec(&payload).expect("payload"))
        );
        AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: parse_chatgpt_jwt_claims(&fake_jwt).expect("jwt"),
                access_token: format!("{account_id}-access"),
                refresh_token: format!("{account_id}-refresh"),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        }
    }

    let codex_home = TempDir::new()?;
    save_auth(
        codex_home.path(),
        &chatgpt_auth("root"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let store = AccountProfileStore::new(codex_home.path().to_path_buf());
    store.ensure_legacy_root_profile(Some("Root".to_string()), /*priority*/ 0)?;

    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    config.model_provider_id = "openai".to_string();
    config.model_provider = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();

    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(execution_auth.ensure_runtime_from_config(&config).await?);
    assert_eq!(execution_auth.account_pool().unwrap().snapshots().len(), 1);

    let added = store.allocate_profile(Some("admin".to_string()), /*priority*/ 10)?;
    save_auth(
        &added.credential_home,
        &chatgpt_auth("admin-account"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    store.complete_profile(&added.id)?;

    assert!(execution_auth.ensure_runtime_from_config(&config).await?);
    let ids = execution_auth
        .account_pool()
        .unwrap()
        .snapshots()
        .into_iter()
        .map(|snapshot| snapshot.profile.id)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&added.id));

    let lease = execution_auth.active_lease().expect("execution lease");
    let mut snapshot = RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(RateLimitWindow {
            used_percent: 99.0,
            window_minutes: Some(300),
            resets_at: None,
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    execution_auth.observe_rate_limits(&lease, &snapshot)?;
    let quota_for_lease = || {
        execution_auth
            .account_pool()
            .unwrap()
            .snapshots()
            .into_iter()
            .find(|snapshot| Some(&snapshot.profile.id) == lease.profile_id())
            .expect("leased profile")
            .rate_limits
    };
    assert_eq!(quota_for_lease(), AccountRateLimits::default());
    snapshot.limit_id = Some("codex".to_string());
    execution_auth.observe_rate_limits(&lease, &snapshot)?;
    assert_eq!(
        quota_for_lease().primary,
        Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: None,
            window_minutes: Some(300),
        })
    );

    // A late entitlement refusal from an old lease cannot block rescue after A -> B -> A.
    let pool = execution_auth.account_pool().expect("installed pool");
    let previous = execution_auth.active_lease().expect("captured request");
    let original = previous.profile_id().expect("pooled request").clone();
    let alternate = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id != original)
        .expect("another profile")
        .profile
        .id;
    pool.activate(&alternate)?;
    pool.activate(&original)?;
    let store = codex_login::AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let before = store.load()?;
    let _ = crate::failover::FailoverCoordinator::handle_inference_error(
        &execution_auth,
        &previous,
        &codex_protocol::error::CodexErr::UsageNotIncluded,
    )
    .await?;
    assert_eq!(store.load()?, before);

    let stored_root = std::fs::read(codex_home.path().join("auth.json"))?;
    let captured_mode = execution_auth
        .mode_for_turn(&config, &config.model_provider)
        .await?;
    execution_auth.suspend_for_logout(&config).await?;
    assert!(captured_mode.capture_binding().is_err());
    execution_auth.compatibility_auth_manager().reload().await;
    assert!(
        execution_auth
            .compatibility_auth_manager()
            .auth_with_http_client_factory()
            .await
            .is_none()
    );
    assert!(AccountPoolRuntime::is_home_suspended(codex_home.path()));
    assert!(execution_auth.active_lease().is_none());
    assert!(execution_auth.account_pool().is_none());
    assert!(!execution_auth.window_warmup_task_running());
    assert!(
        !execution_auth
            .compatibility_auth_manager()
            .has_external_auth()
    );
    assert!(!execution_auth.ensure_runtime_from_config(&config).await?);
    assert!(AccountPoolRuntime::is_home_suspended(codex_home.path()));
    assert_eq!(
        std::fs::read(codex_home.path().join("auth.json"))?,
        stored_root
    );

    assert!(execution_auth.resume_from_config(&config).await?);
    assert!(!AccountPoolRuntime::is_home_suspended(codex_home.path()));
    assert!(
        execution_auth
            .compatibility_auth_manager()
            .has_external_auth()
    );
    assert!(execution_auth.active_lease().is_some());
    assert!(execution_auth.window_warmup_task_running());
    assert_eq!(
        std::fs::read(codex_home.path().join("auth.json"))?,
        stored_root
    );
    Ok(())
}

#[tokio::test]
async fn suspended_pool_reads_stay_logged_out_and_failed_resume_stays_suspended()
-> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("account-profiles.json"),
        "invalid manifest",
    )?;
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    execution_auth.suspend_for_logout(&config).await?;

    assert!(!execution_auth.ensure_runtime_from_config(&config).await?);
    assert!(execution_auth.active_lease().is_none());
    assert!(execution_auth.resume_from_config(&config).await.is_err());
    assert!(AccountPoolRuntime::is_home_suspended(codex_home.path()));
    assert!(execution_auth.account_pool().is_none());
    Ok(())
}

#[tokio::test]
async fn running_warmup_adopts_new_config_without_canceling_the_task() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    let pool = Arc::new(AccountPool::new());
    execution_auth.sync_window_warmup_task(Arc::clone(&pool), &config);
    let (task_id, mut changes) = {
        let guard = execution_auth.window_warmup_task.lock().unwrap();
        let task = guard.as_ref().expect("warmup task");
        (task.handle.id(), task.config_tx.subscribe())
    };

    config.model = Some("new-session-model".to_string());
    config.account_pool.window_warmup_interval_minutes = Some(20);
    execution_auth.sync_window_warmup_task(Arc::clone(&pool), &config);
    assert!(changes.has_changed()?);
    assert_eq!(*changes.borrow_and_update(), config);
    assert_eq!(
        execution_auth
            .window_warmup_task
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .handle
            .id(),
        task_id
    );

    execution_auth.sync_window_warmup_task(Arc::clone(&pool), &config);
    assert!(!changes.has_changed()?);
    config.account_pool.window_warmup = Some(false);
    execution_auth.sync_window_warmup_task(pool, &config);
    assert!(!execution_auth.window_warmup_task_running());
    Ok(())
}
