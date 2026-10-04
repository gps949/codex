//! Request-bound seat and quota notifications for the account pool.

use super::*;
use pretty_assertions::assert_eq;

#[test_case::test_case(false; "available")]
#[test_case::test_case(true; "cooldown_retained")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_logout_preserves_profiles_and_explicit_use_resumes(
    cooling_down: bool,
) -> Result<()> {
    use codex_app_server_protocol::LogoutAccountResponse;
    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/codex/config/bundle"))
        .respond_with(wiremock::ResponseTemplate::new(/*status*/ 200).set_body_json(json!({})))
        .mount(&backend)
        .await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_pool_only_fixture(home.path());
    let credentials = std::fs::read(home.path().join("auth-profiles/selected-acct/auth.json"))?;
    std::fs::write(home.path().join("auth.json"), &credentials)?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    app.initialize().await?;
    let read = app
        .send_raw_request("accountPool/read", /*params*/ None)
        .await?;
    let before: codex_app_server_protocol::AccountPoolReadResponse =
        app.read_response(read).await?;
    assert_eq!(before.active_profile_id.as_deref(), Some("selected-acct"));
    let logout = app.send_logout_account_request().await?;
    let _: LogoutAccountResponse = app.read_response(logout).await?;
    assert!(home.path().join(".account-pool-suspended").is_file());
    let reset = chrono::Utc::now() + chrono::Duration::hours(2);
    if cooling_down {
        let runtime = codex_login::AccountRuntimeStateStore::new(home.path().to_path_buf());
        let mut state = runtime.load()?;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id.as_str() == "selected-acct")
            .expect("selected synthetic profile");
        profile.exhausted_until = Some(reset);
        profile.backend_resets_at = Some(reset);
        runtime.save(&state)?;
    }
    let read = app
        .send_raw_request("accountPool/read", /*params*/ None)
        .await?;
    let paused: codex_app_server_protocol::AccountPoolReadResponse =
        app.read_response(read).await?;
    assert!(paused.enabled);
    assert_eq!(paused.active_profile_id, None);
    assert!(paused.accounts.iter().all(|account| !account.is_active));
    let account = paused
        .accounts
        .iter()
        .find(|account| account.profile_id == "selected-acct")
        .expect("paused synthetic account");
    assert_eq!(
        account.availability,
        if cooling_down {
            codex_app_server_protocol::AccountPoolAvailability::Exhausted {
                resets_at: Some(reset.timestamp()),
            }
        } else {
            codex_app_server_protocol::AccountPoolAvailability::Available
        }
    );
    assert_eq!(
        account.backend_resets_at,
        cooling_down.then_some(reset.timestamp())
    );
    drop(app);
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build()
        .await?;
    app.initialize().await?;
    let account = app
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let restarted: GetAccountResponse = app.read_response(account).await?;
    assert_eq!(restarted.account, None);
    assert!(home.path().join(".account-pool-suspended").is_file());
    let activate = app
        .send_raw_request(
            "accountPool/use",
            Some(json!({"profileId": "selected-acct", "force": cooling_down})),
        )
        .await?;
    let selected: codex_app_server_protocol::AccountPoolUseResponse =
        app.read_response(activate).await?;
    assert_eq!(selected.active_profile_id, "selected-acct");
    assert!(!home.path().join(".account-pool-suspended").exists());
    assert_eq!(
        std::fs::read(home.path().join("auth-profiles/selected-acct/auth.json"))?,
        credentials
    );
    assert_eq!(std::fs::read(home.path().join("auth.json"))?, credentials);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_rate_limit_read_survives_managed_same_seat_token_refresh() -> Result<()> {
    use app_test_support::ChatGptAuthFixture;
    use app_test_support::ChatGptIdTokenClaims;
    use app_test_support::encode_id_token;
    use app_test_support::write_chatgpt_auth;
    use codex_app_server_protocol::GetAccountRateLimitsResponse;
    use codex_config::types::AuthCredentialsStoreMode;
    use std::sync::Arc;
    use tokio::sync::Notify;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({})))
        .mount(&backend)
        .await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("access-original")
            .account_id("shared-workspace")
            .chatgpt_account_id("shared-workspace")
            .chatgpt_user_id("seat-a")
            .plan_type("business"),
        AuthCredentialsStoreMode::File,
    )?;
    let entered = Arc::new(Notify::new());
    let request_entered = Arc::clone(&entered);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-original"))
        .respond_with(move |_: &wiremock::Request| {
            request_entered.notify_one();
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(1))
                .set_body_json(json!({
                    "account_id": "shared-workspace", "user_id": "seat-a", "plan_type": "business",
                    "rate_limit": {"allowed": true, "limit_reached": false,
                        "primary_window": {"used_percent": 43, "limit_window_seconds": 18000,
                            "reset_after_seconds": 3600, "reset_at": 2000000000}}
                }))
        })
        .expect(1)
        .mount(&backend)
        .await;
    let id_token = encode_id_token(
        &ChatGptIdTokenClaims::new()
            .chatgpt_account_id("shared-workspace")
            .chatgpt_user_id("seat-a")
            .plan_type("business"),
    )?;
    Mock::given(method("POST"))
        .and(path("/oauth/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": id_token,
            "access_token": "access-refreshed",
            "refresh_token": "refresh-refreshed"
        })))
        .expect(1)
        .mount(&backend)
        .await;
    let refresh_url = format!("{}/oauth/refresh", backend.uri());
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            ("CODEX_ACCESS_TOKEN", None),
            (
                codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
                Some(refresh_url.as_str()),
            ),
        ])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let read_id = app
        .send_raw_request(
            "account/rateLimits/read",
            Some(json!({
                "excludeResetCreditDetails": true
            })),
        )
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, entered.notified()).await?;
    let refresh_id = app
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(true),
            refresh_token: Some(true),
        })
        .await?;
    let refreshed: GetAuthStatusResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(refresh_id)).await??;
    assert_eq!(refreshed.auth_token.as_deref(), Some("access-refreshed"));
    let response: GetAccountRateLimitsResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(read_id)).await??;
    let snapshot = json!({"limitId": "codex", "planType": "business",
        "primary": {"usedPercent": 43, "windowDurationMins": 300, "resetsAt": 2000000000}});
    assert_eq!(
        response,
        serde_json::from_value::<GetAccountRateLimitsResponse>(json!({
            "ordinaryUsageAllowed": true,
            "accountId": "shared-workspace",
            "rateLimits": snapshot,
            "rateLimitsByLimitId": {"codex": snapshot}
        }))?
    );
    Ok(())
}

#[test_case::test_case(false; "request_seat")]
#[test_case::test_case(true; "preserve_external_selection")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_manual_reset_binds_business_seat(switch_during_reset: bool) -> Result<()> {
    use app_test_support::ChatGptAuthFixture;
    use app_test_support::write_chatgpt_auth;
    use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
    use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditParams;
    use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::AccountProfileId;
    use codex_login::AccountRuntimeStateStore;
    use std::sync::Arc;
    use tokio::sync::Notify;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({})))
        .mount(&backend)
        .await;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = \"{}\"\n[account_pool]\nwindow_warmup = false\n",
            backend.uri(),
        ),
    )?;
    write_models_cache(home.path()).await?;
    write_pool_only_fixture(home.path());
    for (id, user, token) in [
        ("selected-acct", "seat-a", "access-selected"),
        ("other-seat", "seat-b", "access-other"),
    ] {
        let credentials = home.path().join("auth-profiles").join(id);
        std::fs::create_dir_all(&credentials)?;
        write_chatgpt_auth(
            &credentials,
            ChatGptAuthFixture::new(token)
                .account_id("shared-business-workspace")
                .chatgpt_account_id("shared-business-workspace")
                .chatgpt_user_id(user)
                .plan_type("business"),
            AuthCredentialsStoreMode::File,
        )?;
    }
    let manifest_path = home.path().join("account-profiles.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["profiles"]
        .as_array_mut()
        .expect("fixture profile array")
        .push(json!({
            "id": "other-seat", "label": "Other seat", "priority": 10,
            "credential_location": "managed_profile", "state": "ready", "disabled": false
        }));
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    let now = chrono::Utc::now();
    let reset = now + chrono::Duration::hours(5);
    std::fs::write(
        home.path().join("account-runtime-state.json"),
        serde_json::to_vec(&json!({
            "version": 1, "active_profile_id": "selected-acct", "selection_revision": 0,
            "profiles": [
                {"profile_id": "selected-acct", "exhausted_until": reset, "backend_resets_at": reset,
                    "rate_limits": {"observed_at": now,
                    "primary": {"used_percent": 100.0, "resets_at": reset, "window_minutes": 300}, "secondary": null}},
                {"profile_id": "other-seat", "exhausted_until": reset, "rate_limits": {"observed_at": now,
                    "primary": {"used_percent": 73.0, "resets_at": reset, "window_minutes": 300}, "secondary": null}}
            ]
        }))?,
    )?;
    let entered = Arc::new(Notify::new());
    let request_entered = Arc::clone(&entered);
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "available_count": 1, "credits": [{"id": "business-credit", "reset_type": "codex_rate_limits",
                "status": "available", "granted_at": "2026-01-01T00:00:00Z", "expires_at": null}]
        })))
        .mount(&backend).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("authorization", "Bearer access-selected"))
        .and(header("chatgpt-account-id", "shared-business-workspace"))
        .respond_with(move |_: &wiremock::Request| {
            request_entered.notify_one();
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(if switch_during_reset { 3 } else { 0 }))
                .set_body_json(json!({"code": "reset", "windows_reset": 2}))
        })
        .expect(1)
        .mount(&backend)
        .await;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let account_read = app
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let _: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(account_read)).await??;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let mut expected = store.load()?;
    let consume = app
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "business-seat-reset".into(),
                credit_id: None,
            },
        )
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, entered.notified()).await?;
    if switch_during_reset {
        let selected = AccountProfileId::new("other-seat")?;
        store.select(
            selected.clone(),
            codex_login::AccountSelectionMode::ForceProbe,
        )?;
        timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_stream_until_matching_notification(
                "concurrent account selection",
                |notification| {
                    notification.method == "accountPool/updated"
                        && notification
                            .params
                            .as_ref()
                            .and_then(|params| params["activeProfileId"].as_str())
                            == Some("other-seat")
                },
            ),
        )
        .await??;
        expected.active_profile_id = Some(selected);
        expected.selection_revision += 1;
        expected
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id.as_str() == "other-seat")
            .expect("fixture other seat")
            .exhausted_until = None;
    }
    let response: ConsumeAccountRateLimitResetCreditResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(consume)).await??;
    assert_eq!(
        response,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    let mut actual = store.load()?;
    let recovered = actual
        .profiles
        .iter()
        .find(|profile| profile.profile_id.as_str() == "selected-acct")
        .expect("fixture selected seat");
    let reset_at = recovered
        .quota_reset_at
        .expect("redeemed request seat reset");
    let observed_at = recovered
        .quota_reset_observed_at
        .expect("request-bound observation");
    let restored = expected
        .profiles
        .iter_mut()
        .find(|profile| profile.profile_id.as_str() == "selected-acct")
        .expect("fixture selected seat");
    restored.quota_reset_at = Some(reset_at);
    restored.quota_reset_observed_at = Some(observed_at);
    restored.exhausted_until = None;
    restored.backend_resets_at = None;
    restored.rate_limits = codex_login::AccountRateLimits {
        observed_at: Some(observed_at + chrono::Duration::nanoseconds(1)),
        ..codex_login::AccountRateLimits::default()
    };
    actual
        .profiles
        .sort_by(|left, right| left.profile_id.as_str().cmp(right.profile_id.as_str()));
    expected
        .profiles
        .sort_by(|left, right| left.profile_id.as_str().cmp(right.profile_id.as_str()));
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_quota_only_updates_publish_stable_quota_without_identity_reset() -> Result<()>
{
    use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
    use codex_login::AccountRateLimitWindow;
    use codex_login::AccountRateLimits;
    use codex_login::AccountRuntimeStateStore;
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let home = TempDir::new()?;
    let backend = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&backend).await;
    create_config_toml(home.path(), Some(&backend.uri()))?;
    write_models_cache(home.path()).await?;
    write_pool_only_fixture(home.path());
    let manifest_path = home.path().join("account-profiles.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest["profiles"][0]["label"] = json!("Work");
    std::fs::write(manifest_path, serde_json::to_vec(&manifest)?)?;
    let reset = chrono::Utc::now() + chrono::Duration::hours(5);
    let reset_timestamp = reset.timestamp();
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "pro", "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 12, "limit_window_seconds": 18000,
                    "reset_after_seconds": 18000, "reset_at": reset_timestamp}}
        })))
        .expect(1)
        .mount(&backend)
        .await;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let read_id = app.send_raw_request("accountPool/read", None).await?;
    let _: codex_app_server_protocol::AccountPoolReadResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(read_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("initial quota observation", |notification| {
            notification.method == "accountPool/updated"
                && notification.params.as_ref().is_some_and(|params| {
                    params["accounts"][0]["rateLimits"]["primary"]["usedPercent"] == json!(12.0)
                })
        }),
    )
    .await??;
    app.clear_message_buffer();
    AccountRuntimeStateStore::new(home.path().to_path_buf()).record_rate_limits(
        &codex_login::AccountProfileId::new("selected-acct")?,
        AccountRateLimits {
            primary: Some(AccountRateLimitWindow {
                used_percent: 62.0,
                resets_at: Some(reset),
                window_minutes: Some(300),
            }),
            secondary: None,
            observed_at: Some(chrono::Utc::now()),

            window_observed_at: None,
        },
    )?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("updated quota observation", |notification| {
            notification.method == "accountPool/updated"
                && notification.params.as_ref().is_some_and(|params| {
                    params["accounts"][0]["rateLimits"]["primary"]["usedPercent"] == json!(62.0)
                })
        }),
    )
    .await??;
    assert!(
        !app.pending_notification_methods()
            .iter()
            .any(|method| method == "account/updated")
    );
    let quota: AccountRateLimitsUpdatedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_notification("account/rateLimits/updated"),
    )
    .await??;
    assert_eq!(
        quota,
        serde_json::from_value::<AccountRateLimitsUpdatedNotification>(json!({
            "rateLimits": {"limitId": "codex", "limitName": "Work · Current quota", "planType": "pro",
                "primary": {"usedPercent": 62, "windowDurationMins": 300, "resetsAt": reset_timestamp}}
        }))?
    );
    Ok(())
}
