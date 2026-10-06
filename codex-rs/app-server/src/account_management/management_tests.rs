use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn legacy_reset_review_archives_only_the_inspected_operation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let name = format!(".rate-limit-reset-credit-{}.json", "a".repeat(40));
    let bytes = br#"{"profileId":"synthetic-profile","resetKey":123,"quotaEpoch":null,"attemptedAt":1,"requestId":"synthetic-request"}"#;
    std::fs::write(home.path().join(&name), bytes)?;
    let inventory = manager.inventory().await?;
    let record = &inventory.reset_journals[0];
    assert!(record.archive_available);
    let result = manager
        .execute(AccountManagerOperation::ResetJournalArchive {
            file_name: record.file_name.clone(),
            expected_digest: record.digest.clone(),
            acknowledge_unconfirmed: true,
        })
        .await?;
    assert!(result.message.contains("archived"));
    assert!(manager.inventory().await?.reset_journals.is_empty());
    let directory = std::fs::read_dir(home.path().join(".reset-credit-journal-archive"))?
        .next()
        .expect("archive")?
        .path();
    assert_eq!(std::fs::read(directory.join(name))?, bytes);
    Ok(())
}

#[tokio::test]
async fn unreadable_reset_inventory_does_not_block_account_management() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    for index in 0..129 {
        std::fs::write(
            home.path()
                .join(format!(".rate-limit-reset-credit-{index:040x}.json")),
            b"{interrupted",
        )?;
    }
    let inventory = AccountManager::new(config).inventory().await?;
    assert_eq!(
        (
            inventory.accounts.len(),
            inventory.reset_journals.len(),
            inventory.reset_journals[0].archive_available
        ),
        (0, 1, false)
    );
    assert!(
        inventory.reset_journals[0]
            .message
            .contains("Other account management remains available")
    );
    Ok(())
}

#[tokio::test]
async fn account_manager_removal_reports_committed_metadata_when_scheduler_cleanup_fails()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(/*label*/ None, /*priority*/ 10)?;
    save_profile_email(&profile, "retained@example.com")?;
    store.complete_profile(&profile.id)?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    std::fs::write(
        home.path().join("account-runtime-state.json"),
        "invalid scheduler data",
    )?;
    let result = AccountManager::new(config)
        .execute(AccountManagerOperation::Remove {
            profile_id: profile.id.to_string(),
            keep_credentials: true,
        })
        .await?;
    assert!(store.load_profile_records()?.is_empty());
    assert!(profile.credential_home.join("auth.json").exists());
    assert_eq!(
        (
            result.data["removedFromPool"].as_bool(),
            result.data["credentialsRemoved"].as_bool(),
            result.data["credentialsRetained"].as_bool()
        ),
        (Some(true), Some(false), Some(true))
    );
    assert!(result.message.contains("some cleanup remains"));
    assert_eq!(result.data["cleanupWarnings"].as_array().unwrap().len(), 1);
    Ok(())
}

fn save_profile_email(profile: &codex_login::AccountProfile, email: &str) -> anyhow::Result<()> {
    use base64::Engine as _;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "email": email, "https://api.openai.com/auth": {
                "chatgpt_user_id": "fixture-owner", "chatgpt_account_id": "fixture-account",
                "chatgpt_plan_type": "pro"
            }
        })
        .to_string(),
    );
    std::fs::write(
        profile.credential_home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "tokens": {"id_token": format!("e30.{claims}.sig"), "access_token": "synthetic-access", "refresh_token": "synthetic-refresh", "account_id": "fixture-account"},
            "last_refresh": "2099-01-01T00:00:00Z"
        }))?,
    )?;
    Ok(())
}

#[tokio::test]
async fn account_manager_names_track_email_without_persisting_derived_names() -> anyhow::Result<()>
{
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(/*label*/ None, /*priority*/ 10)?;
    save_profile_email(&profile, "original@example.com")?;
    store.complete_profile(&profile.id)?;
    let pending = store.allocate_profile(/*label*/ None, /*priority*/ 20)?;
    save_profile_email(&pending, "pending@example.com")?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let inventory = manager.inventory().await?;
    assert_eq!(
        (
            inventory.accounts[0].label.as_str(),
            inventory.accounts[0].custom_label.as_deref()
        ),
        ("original@example.com", None)
    );
    assert_eq!(
        (
            inventory.accounts[1].label.as_str(),
            inventory.accounts[1].email.as_deref()
        ),
        (pending.id.as_str(), None)
    );
    manager
        .execute(AccountManagerOperation::Update {
            profile_id: profile.id.to_string(),
            label: None,
            priority: Some(11),
            disabled: None,
        })
        .await?;
    assert_eq!(store.load_profile_records()?[0].profile.label, None);
    manager
        .execute(AccountManagerOperation::Update {
            profile_id: profile.id.to_string(),
            label: Some("Work".into()),
            priority: None,
            disabled: None,
        })
        .await?;
    save_profile_email(&profile, "new@example.com")?;
    let inventory = manager.inventory().await?;
    assert_eq!(
        (
            inventory.accounts[0].label.as_str(),
            inventory.accounts[0].custom_label.as_deref(),
            inventory.accounts[0].email.as_deref()
        ),
        ("Work", Some("Work"), Some("new@example.com"))
    );
    manager
        .execute(AccountManagerOperation::Update {
            profile_id: profile.id.to_string(),
            label: Some(String::new()),
            priority: None,
            disabled: None,
        })
        .await?;
    let inventory = manager.inventory().await?;
    assert_eq!(
        (
            inventory.accounts[0].label.as_str(),
            inventory.accounts[0].custom_label.as_deref()
        ),
        ("new@example.com", None)
    );
    assert_eq!(store.load_profile_records()?[0].profile.label, None);
    assert_eq!(
        serde_json::to_value(&inventory.accounts[0])?["customLabel"],
        serde_json::Value::Null
    );
    Ok(())
}

#[tokio::test]
async fn account_manager_inventory_remains_available_without_usable_accounts() -> anyhow::Result<()>
{
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(Some("Pending account".into()), 10)?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let inventory = manager.inventory().await?;
    assert_eq!(
        (
            inventory.accounts.len(),
            inventory.accounts[0].profile_id.as_str(),
            inventory.accounts[0].login_state.as_str()
        ),
        (1, profile.id.as_str(), "pending")
    );
    assert_eq!(inventory.active_profile_id, None);
    Ok(())
}

#[test]
fn account_manager_operations_distinguish_probe_from_redemption() {
    let operation: AccountManagerOperation =
        serde_json::from_value(serde_json::json!({"type":"retry","profileId":"fixture"}))
            .expect("retry operation");
    assert!(
        matches!(operation, AccountManagerOperation::Retry { profile_id } if profile_id == "fixture")
    );
}

#[tokio::test]
async fn account_management_refresh_confirms_external_reset_without_generating_requests()
-> anyhow::Result<()> {
    use base64::Engine as _;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    let home = tempfile::TempDir::new()?;
    let server = MockServer::start().await;
    let reset = chrono::Utc::now() + chrono::Duration::hours(2);
    Mock::given(method("GET")).and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(400)).set_body_json(serde_json::json!({
            "account_id":"fixture-account", "user_id":"fixture-owner", "plan_type":"pro",
            "rate_limit":{"allowed":true,"limit_reached":false,
                "primary_window":{"used_percent":0,"limit_window_seconds":18000,"reset_at":reset.timestamp(),"reset_after_seconds":7200},
                "secondary_window":{"used_percent":0,"limit_window_seconds":604800,"reset_at":reset.timestamp(),"reset_after_seconds":7200}},
            "spend_control":{"reached":false}
        }))).expect(2).mount(&server).await;
    app_test_support::mount_workspace_routing(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(Some("Synthetic work".into()), /*priority*/ 10)?;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::json!({
        "https://api.openai.com/auth":{"chatgpt_user_id":"fixture-owner","chatgpt_account_id":"fixture-account"}
    }).to_string());
    std::fs::write(
        profile.credential_home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "tokens":{"id_token":format!("e30.{claims}.sig"),"access_token":"synthetic-access","refresh_token":"synthetic-refresh","account_id":"fixture-account"},
            "last_refresh":"2099-01-01T00:00:00Z"
        }))?,
    )?;
    store.complete_profile(&profile.id)?;
    let runtime = codex_login::AccountRuntimeStateStore::new(home.path().to_path_buf());
    runtime.save(&serde_json::from_value(serde_json::json!({"active_profile_id":profile.id,"profiles":[{
        "profile_id":profile.id,"exhausted_until":reset,"backend_resets_at":reset,
        "rate_limits":{"primary":{"used_percent":100,"resets_at":reset,"window_minutes":300},
            "secondary":{"used_percent":100,"resets_at":reset,"window_minutes":10080},"observed_at":chrono::Utc::now()}
    }]}))?)?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    let manager = AccountManager::new(config);
    let refreshing = {
        let manager = Arc::clone(&manager);
        let id = profile.id.to_string();
        tokio::spawn(async move {
            manager
                .execute(AccountManagerOperation::Refresh {
                    profile_ids: Some(vec![id.clone(), id]),
                })
                .await
        })
    };
    let refresh_key = (
        profile.id.to_string(),
        manager.profile_identity(profile.id.as_str())?,
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if manager
                .refreshes
                .lock()
                .unwrap()
                .get(&refresh_key)
                .is_some_and(|status| status.in_progress)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let joined = manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![profile.id.to_string()]),
        })
        .await?;
    assert!(
        joined.message.contains("1 already checking"),
        "{}",
        joined.message
    );
    refreshing.await??;
    let inventory = manager.inventory().await?;
    assert!(
        inventory.accounts[0].refresh.as_ref().unwrap().succeeded,
        "{:?}",
        inventory.accounts[0].refresh
    );
    assert_eq!(
        (
            inventory.accounts[0].availability.as_str(),
            inventory.accounts[0]
                .rate_limits
                .primary
                .as_ref()
                .map(|window| window.used_percent)
        ),
        ("ready", Some(0.0))
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() == "GET")
    );
    let interrupted = {
        let manager = Arc::clone(&manager);
        let id = profile.id.to_string();
        tokio::spawn(async move {
            manager
                .execute(AccountManagerOperation::Refresh {
                    profile_ids: Some(vec![id]),
                })
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path() == "/backend-api/wham/usage")
                .count()
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    interrupted.abort();
    assert!(matches!(interrupted.await, Err(error) if error.is_cancelled()));
    let latest = manager.inventory().await?;
    assert!(!latest.accounts[0].refresh.as_ref().unwrap().in_progress);
    assert!(
        latest.accounts[0]
            .refresh
            .as_ref()
            .unwrap()
            .message
            .contains("interrupted")
    );
    assert_eq!(
        latest.accounts[0]
            .rate_limits
            .primary
            .as_ref()
            .map(|window| window.used_percent),
        Some(0.0)
    );
    assert_eq!(
        runtime.load()?.active_profile_id.as_ref(),
        Some(&profile.id)
    );
    Ok(())
}

#[tokio::test]
async fn account_manager_primary_operations_keep_inference_and_credentials_independent()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(/*label*/ None, /*priority*/ 10)?;
    save_profile_email(&profile, "remote@example.com")?;
    store.complete_profile(&profile.id)?;
    let auth_before = std::fs::read(profile.credential_home.join("auth.json"))?;
    let manifest_before = std::fs::read(store.manifest_path())?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let use_host: AccountManagerOperation =
        serde_json::from_value(serde_json::json!({"type": "primaryUse", "profileId": profile.id}))?;
    manager.execute(use_host).await?;
    let view = manager
        .inventory()
        .await?
        .primary_login
        .expect("primary view");
    assert_eq!(
        (
            view.source,
            view.profile_id,
            view.label,
            view.email,
            view.ready
        ),
        (
            "profile".into(),
            Some(profile.id.to_string()),
            "remote@example.com".into(),
            Some("remote@example.com".into()),
            true
        )
    );
    assert!(!home.path().join("auth.json").exists());
    assert!(!home.path().join("account-runtime-state.json").exists());
    assert_eq!(std::fs::read(store.manifest_path())?, manifest_before);
    assert_eq!(
        std::fs::read(profile.credential_home.join("auth.json"))?,
        auth_before
    );
    assert!(
        manager
            .execute(AccountManagerOperation::Remove {
                profile_id: profile.id.to_string(),
                keep_credentials: true
            })
            .await
            .is_err()
    );
    manager
        .execute(AccountManagerOperation::PrimaryLogout)
        .await?;
    assert_eq!(
        manager.inventory().await?.primary_login.unwrap().source,
        "signedOut"
    );
    manager
        .execute(AccountManagerOperation::PrimaryRoot)
        .await?;
    assert_eq!(
        manager.inventory().await?.primary_login.unwrap().source,
        "root"
    );
    assert_eq!(
        std::fs::read(profile.credential_home.join("auth.json"))?,
        auth_before
    );
    Ok(())
}

#[tokio::test]
async fn account_manager_inventory_and_quota_use_persisted_seats_under_external_auth()
-> anyhow::Result<()> {
    use base64::Engine as _;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    let home = tempfile::TempDir::new()?;
    let server = MockServer::start().await;
    app_test_support::mount_workspace_routing(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let mut profiles = Vec::new();
    let external_claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "email": "unrelated@example.com", "https://api.openai.com/auth": {
                "chatgpt_user_id": "unrelated-user", "chatgpt_account_id": "unrelated-workspace"
            }
        })
        .to_string(),
    );
    for user in ["first", "second"] {
        let profile = store.allocate_profile(/*label*/ None, /*priority*/ 10)?;
        let stored_claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "email": format!("{user}@example.com"), "https://api.openai.com/auth": {
                    "chatgpt_user_id": user, "chatgpt_account_id": "shared-workspace"
                }
            })
            .to_string(),
        );
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&serde_json::json!({
                "tokens": {"id_token": format!("e30.{stored_claims}.sig"),
                    "access_token": format!("access-{user}"), "refresh_token": format!("refresh-{user}"),
                    "account_id": "shared-workspace"}, "last_refresh": "2099-01-01T00:00:00Z"
            }))?,
        )?;
        store.complete_profile(&profile.id)?;
        codex_login::auth::login_with_chatgpt_auth_tokens(
            &profile.credential_home,
            &format!("e30.{external_claims}.sig"),
            "unrelated-workspace",
            Some("pro"),
        )?;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .and(header("Authorization", format!("Bearer access-{user}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "account_id": "shared-workspace", "user_id": user, "plan_type": "business",
                "rate_limit": {"allowed": true, "limit_reached": false, "primary_window": {
                    "used_percent": 12, "limit_window_seconds": 18000,
                    "reset_at": chrono::Utc::now().timestamp() + 7200, "reset_after_seconds": 7200
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;
        profiles.push(profile);
    }
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    let manager = AccountManager::new(config);
    let inventory = manager.inventory().await?;
    let mut names: Vec<_> = inventory
        .accounts
        .into_iter()
        .map(|account| (account.label, account.email, account.login_state))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            (
                "first@example.com".into(),
                Some("first@example.com".into()),
                "signedIn".into()
            ),
            (
                "second@example.com".into(),
                Some("second@example.com".into()),
                "signedIn".into()
            )
        ]
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(
                profiles
                    .iter()
                    .map(|profile| profile.id.to_string())
                    .collect(),
            ),
        })
        .await?;
    let refreshed = manager.inventory().await?;
    assert!(
        refreshed.accounts.iter().all(|account| account
            .refresh
            .as_ref()
            .is_some_and(|refresh| refresh.succeeded)),
        "Refresh outcomes: {:?}; request paths: {:?}",
        refreshed
            .accounts
            .iter()
            .map(|account| (&account.label, &account.refresh))
            .collect::<Vec<_>>(),
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| request.url.path().to_string())
            .collect::<Vec<_>>()
    );
    assert!(
        !home.path().join("account-runtime-state.json").exists()
            || codex_login::AccountRuntimeStateStore::new(home.path().to_path_buf())
                .load()?
                .active_profile_id
                .is_none()
    );
    Ok(())
}
