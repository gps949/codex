use super::*;
use pretty_assertions::assert_eq;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "account_id":"fixture-account", "user_id":"fixture-owner", "plan_type":"pro",
            "rate_limit":{"allowed":true,"limit_reached":false,
                "primary_window":{"used_percent":0,"limit_window_seconds":18000,"reset_at":reset.timestamp(),"reset_after_seconds":7200},
                "secondary_window":{"used_percent":0,"limit_window_seconds":604800,"reset_at":reset.timestamp(),"reset_after_seconds":7200}},
            "spend_control":{"reached":false}
        }))).mount(&server).await;
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
    manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![profile.id.to_string()]),
        })
        .await?;
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
    assert_eq!(
        runtime.load()?.active_profile_id.as_ref(),
        Some(&profile.id)
    );
    Ok(())
}
