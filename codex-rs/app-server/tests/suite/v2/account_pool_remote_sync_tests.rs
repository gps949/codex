//! Stable mobile account views during delayed reads and standby-only pool updates.

use super::*;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::AccountPoolUseResponse;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::WarningNotification;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AccountRuntimeStateStore;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

fn write_mobile_pool(home: &Path) -> Result<()> {
    write_pool_only_fixture(home);
    write_profile_credentials(home, "backup", "access-backup");
    write_profile_credentials(home, "third", "access-third");
    let manifest_path = home.join("account-profiles.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["profiles"][0]["label"] = json!("Work");
    manifest["profiles"]
        .as_array_mut()
        .expect("fixture profile array")
        .push(json!({
            "id": "backup", "label": "Backup", "priority": 10,
            "credential_location": "managed_profile", "state": "ready", "disabled": false
        }));
    manifest["profiles"]
        .as_array_mut()
        .expect("fixture profile array")
        .push(json!({
            "id": "third", "label": "Travel", "priority": 20,
            "credential_location": "managed_profile", "state": "ready", "disabled": false
        }));
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_account_read_rechecks_owner_after_final_pool_identity_load() -> Result<()> {
    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_mobile_pool(home.path())?;
    let standby_auth_path = home.path().join("auth-profiles/selected-acct/auth.json");
    let mut standby_auth: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&standby_auth_path)?)?;
    standby_auth["last_refresh"] = json!(chrono::Utc::now() - chrono::Duration::days(30));
    let standby_auth = serde_json::to_vec(&standby_auth)?;
    Mock::given(method("GET"))
        .and(path_regex("^/(backend-api/wham|api/codex)/accounts/check$"))
        .respond_with(move |request: &wiremock::Request| {
            let account_id = request.headers["chatgpt-account-id"].to_str().unwrap();
            if account_id == "account-backup" {
                // Routing completes before the second pool read. Only that final read
                // sees the aged standby credentials and waits for their refresh.
                std::fs::write(&standby_auth_path, &standby_auth).unwrap();
            }
            ResponseTemplate::new(/*status_code*/ 200).set_body_json(json!({
                "accounts": [{"id": account_id, "workspace_backend_origin": "https://chatgpt.com",
                    "account_routing_override": "NO_CONSTRAINT"}]
            }))
        })
        .mount(&backend)
        .await;
    let entered = Arc::new(Notify::new());
    let request_entered = Arc::clone(&entered);
    Mock::given(method("POST"))
        .and(path("/oauth/refresh"))
        .and(body_partial_json(
            json!({"refresh_token": "refresh-selected-acct"}),
        ))
        .respond_with(move |_: &wiremock::Request| {
            request_entered.notify_one();
            ResponseTemplate::new(/*status_code*/ 200)
                .set_delay(Duration::from_secs(/*secs*/ 3))
                .set_body_json(json!({"access_token": "access-selected-refreshed"}))
        })
        .mount(&backend)
        .await;
    let refresh_url = format!("{}/oauth/refresh", backend.uri());
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (
                codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
                Some(&refresh_url),
            ),
        ])
        .build()
        .await?;
    app.initialize_with_client_info(ClientInfo {
        name: "codex_chatgpt_ios_remote".into(),
        title: None,
        version: "1.0".into(),
    })
    .await?;
    let initial_id = app
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let _: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(initial_id)).await??;
    let use_id = app
        .send_raw_request("accountPool/use", Some(json!({"profileId": "backup"})))
        .await?;
    let _: AccountPoolUseResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(use_id)).await??;
    let read_id = app
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, entered.notified()).await?;
    AccountRuntimeStateStore::new(home.path().to_path_buf()).select(
        AccountProfileId::new("third")?,
        codex_login::AccountSelectionMode::AvailableOnly,
    )?;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(read_id)),
    )
    .await??;
    assert_eq!(
        error.error,
        codex_app_server_protocol::JSONRPCErrorError {
            code: -32603,
            message: "account changed during workspace routing discovery".into(),
            data: None,
        }
    );
    let fresh_id = app
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let fresh: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(fresh_id)).await??;
    assert_eq!(
        fresh
            .account_pool
            .as_ref()
            .unwrap()
            .active_profile_id
            .as_deref(),
        Some("third")
    );
    assert_eq!(
        fresh.workspace_routing.unwrap().chatgpt_account_id,
        "account-third"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_standby_updates_publish_deduplicated_summary_without_identity_reset() -> Result<()>
{
    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_mobile_pool(home.path())?;
    let reset = chrono::Utc::now() + chrono::Duration::hours(5);
    for (access, used) in [
        ("access-selected", 12),
        ("access-backup", 23),
        ("access-third", 33),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .and(header("authorization", format!("Bearer {access}")))
            .respond_with(
                ResponseTemplate::new(/*status_code*/ 200).set_body_json(json!({
                    "plan_type": "pro", "rate_limit": {"allowed": true, "limit_reached": false,
                        "primary_window": {"used_percent": used, "limit_window_seconds": 18000,
                            "reset_after_seconds": 18000, "reset_at": reset.timestamp()}}
                })),
            )
            .mount(&backend)
            .await;
    }
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build()
        .await?;
    app.initialize_with_client_info(ClientInfo {
        name: "codex_chatgpt_ios_remote".into(),
        title: None,
        version: "1.0".into(),
    })
    .await?;
    let id = app.send_raw_request("accountPool/read", None).await?;
    let _: AccountPoolReadResponse = timeout(DEFAULT_READ_TIMEOUT, app.read_response(id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("initial standby quota", |notification| {
            notification.method == "accountPool/updated"
                && notification.params.as_ref().is_some_and(|params| {
                    params["accounts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|account| {
                            account["profileId"] == "backup"
                                && account["rateLimits"]["primary"]["usedPercent"] == 23.0
                        })
                })
        }),
    )
    .await??;
    app.clear_message_buffer();
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let observed = chrono::Utc::now();
    let limits = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 62.0,
            resets_at: Some(reset),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(observed),

        window_observed_at: None,
    };
    store.record_rate_limits(&AccountProfileId::new("backup")?, limits.clone())?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("standby quota change", |notification| {
            notification.method == "accountPool/updated"
                && notification.params.as_ref().is_some_and(|params| {
                    params["accounts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|account| {
                            account["profileId"] == "backup"
                                && account["rateLimits"]["primary"]["usedPercent"] == 62.0
                        })
                })
        }),
    )
    .await??;
    let warning: WarningNotification =
        timeout(DEFAULT_READ_TIMEOUT, app.read_notification("warning")).await??;
    assert!(
        warning
            .message
            .contains("Backup · Ready · Primary 62% used"),
        "{}",
        warning.message
    );
    assert!(warning.message.chars().count() <= 280);
    assert!(
        !app.pending_notification_methods()
            .iter()
            .any(|method| matches!(
                method.as_str(),
                "account/updated" | "account/rateLimits/updated"
            ))
    );
    app.clear_message_buffer();
    let later = observed + chrono::Duration::minutes(1);
    store.record_rate_limits(
        &AccountProfileId::new("backup")?,
        AccountRateLimits {
            observed_at: Some(later),
            ..limits
        },
    )?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification(
            "standby observation timestamp",
            |notification| {
                notification.method == "accountPool/updated"
                    && notification.params.as_ref().is_some_and(|params| {
                        params["accounts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|account| {
                                account["profileId"] == "backup"
                                    && account["rateLimits"]["observedAt"] == later.timestamp()
                            })
                    })
            },
        ),
    )
    .await??;
    assert!(
        !app.pending_notification_methods()
            .iter()
            .any(|method| matches!(
                method.as_str(),
                "warning" | "account/updated" | "account/rateLimits/updated"
            ))
    );
    app.clear_message_buffer();
    let manifest_path = home.path().join("account-profiles.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["profiles"][1]["label"] = json!("Backup Team");
    manifest["profiles"][1]["disabled"] = json!(true);
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification(
            "standby label and availability change",
            |notification| {
                notification.method == "accountPool/updated"
                    && notification.params.as_ref().is_some_and(|params| {
                        params["accounts"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|account| {
                                account["profileId"] == "backup"
                                    && account["label"] == "Backup Team"
                                    && account["availability"]["type"] == "disabled"
                            })
                    })
            },
        ),
    )
    .await??;
    let warning: WarningNotification =
        timeout(DEFAULT_READ_TIMEOUT, app.read_notification("warning")).await??;
    assert!(
        warning.message.contains("Backup Team · Disabled"),
        "{}",
        warning.message
    );
    assert!(
        !app.pending_notification_methods()
            .iter()
            .any(|method| matches!(
                method.as_str(),
                "account/updated" | "account/rateLimits/updated"
            ))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mobile_workspace_message_read_discards_old_owner_after_switch() -> Result<()> {
    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_mobile_pool(home.path())?;
    let entered = Arc::new(Notify::new());
    let request_entered = Arc::clone(&entered);
    Mock::given(method("GET"))
        .and(path("/api/codex/workspace-messages"))
        .respond_with(move |_: &wiremock::Request| {
            request_entered.notify_one();
            ResponseTemplate::new(/*status_code*/ 200)
                .set_delay(Duration::from_secs(/*secs*/ 2))
                .set_body_json(json!({"messages": []}))
        })
        .mount(&backend)
        .await;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build()
        .await?;
    app.initialize_with_client_info(ClientInfo {
        name: "codex_chatgpt_ios_remote".into(),
        title: None,
        version: "1.0".into(),
    })
    .await?;
    let read_id = app
        .send_raw_request("account/workspaceMessages/read", None)
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, entered.notified()).await?;
    let use_id = app
        .send_raw_request("accountPool/use", Some(json!({"profileId": "backup"})))
        .await?;
    let _: AccountPoolUseResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(use_id)).await??;
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_error_message(RequestId::Integer(read_id)),
    )
    .await??;
    assert_eq!(
        error.error,
        codex_app_server_protocol::JSONRPCErrorError {
            code: -32603,
            data: None,
            message: "account changed while reading workspace messages; retry the request".into(),
        }
    );
    Ok(())
}
