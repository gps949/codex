use std::path::Path;

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditParams;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::RateLimitWindow;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(/*secs*/ 10);
const RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR: &str =
    "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";
const SERVER_TIMEOUT_READ_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(/*secs*/ 15);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INTERNAL_ERROR_CODE: i64 = -32603;

#[tokio::test]
async fn consume_rate_limit_reset_credit_requires_chatgpt_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let consume_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                expected_owner_key: None,
                idempotency_key: "request-1".to_string(),
                credit_id: None,
            },
        )
        .await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "codex account authentication required for rate limit reset credits"
    );

    login_with_api_key(&mut mcp, "sk-test-key").await?;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-2").await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "chatgpt authentication required for rate limit reset credits"
    );
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_maps_backend_outcomes() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    let cases = [
        (
            "request-reset",
            "reset",
            ConsumeAccountRateLimitResetCreditOutcome::Reset,
            2,
        ),
        (
            "request-nothing",
            "nothing_to_reset",
            ConsumeAccountRateLimitResetCreditOutcome::NothingToReset,
            0,
        ),
        (
            "request-no-credit",
            "no_credit",
            ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
            0,
        ),
        (
            "request-retry",
            "already_redeemed",
            ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed,
            0,
        ),
    ];
    for (idempotency_key, backend_code, _, windows_reset) in cases {
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(header("authorization", "Bearer chatgpt-token"))
            .and(header("chatgpt-account-id", "account-123"))
            .and(body_json(
                json!({ "redeem_request_id": idempotency_key, "credit_id": "credit-123" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": backend_code,
                "windows_reset": windows_reset
            })))
            .mount(&server)
            .await;
    }

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    for (idempotency_key, _, expected_outcome, _) in cases {
        assert_eq!(
            consume_reset_credit(&mut mcp, idempotency_key).await?,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: expected_outcome,
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_forwards_selected_credit_id() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("authorization", "Bearer chatgpt-token"))
        .and(header("chatgpt-account-id", "account-123"))
        .and(body_json(json!({
            "redeem_request_id": "request-selected",
            "credit_id": "credit-123",
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                expected_owner_key: None,
                idempotency_key: "request-selected".to_string(),
                credit_id: Some("credit-123".to_string()),
            },
        )
        .await?;

    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<ConsumeAccountRateLimitResetCreditResponse>(request_id),
        )
        .await??,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_rejects_empty_idempotency_key() -> Result<()> {
    let (codex_home, _server) = chatgpt_test_context().await?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                expected_owner_key: None,
                idempotency_key: String::new(),
                credit_id: None,
            },
        )
        .await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "idempotencyKey must not be empty");
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_rejects_empty_credit_id() -> Result<()> {
    let (codex_home, _server) = chatgpt_test_context().await?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                expected_owner_key: None,
                idempotency_key: "request-1".to_string(),
                credit_id: Some(String::new()),
            },
        )
        .await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "creditId must not be empty");
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_surfaces_backend_failure() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    let request_id = send_consume_reset_credit(&mut mcp, "request-1").await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INTERNAL_ERROR_CODE);
    assert!(
        error
            .error
            .message
            .contains("failed to consume rate limit reset"),
        "unexpected error message: {}",
        error.error.message
    );
    Ok(())
}

#[tokio::test]
async fn consume_timeout_releases_account_auth_queue() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/accounts/check"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"accounts": [{
                "id": "account-123", "workspace_backend_origin": "https://chatgpt.com",
                "account_routing_override": "NO_CONSTRAINT"
            }]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(/*secs*/ 1))
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .mount(&server)
        .await;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR, Some("100")),
        ])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-timeout").await?;
    let account_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;

    let consume_error: JSONRPCError = timeout(
        SERVER_TIMEOUT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(consume_id)),
    )
    .await??;
    assert_eq!(consume_error.error.code, INTERNAL_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "rate limit reset consume timed out"
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(account_id)),
    )
    .await??;
    Ok(())
}

#[tokio::test]
async fn consume_preserves_new_refusals_and_unconfirmed_partial_resets() -> Result<()> {
    for (code, windows, new_refusal) in [
        ("reset", 2, true),
        ("reset", 1, false),
        ("nothing_to_reset", 0, false),
        ("already_redeemed", 0, false),
    ] {
        let (home, server) = chatgpt_test_context().await?;
        app_test_support::mount_workspace_routing(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/codex/config/bundle"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let profiles = codex_login::AccountProfileStore::new(home.path().to_path_buf());
        let profile =
            profiles.allocate_profile(Some("Synthetic seat".into()), /*priority*/ 0)?;
        std::fs::copy(
            home.path().join("auth.json"),
            profile.credential_home.join("auth.json"),
        )?;
        profiles.complete_profile(&profile.id)?;
        let reset = chrono::Utc::now() + chrono::Duration::hours(2);
        std::fs::write(
            home.path().join("account-runtime-state.json"),
            serde_json::to_vec(&json!({
                "version": 1, "active_profile_id": profile.id.as_str(), "profiles": [{
                    "profile_id": profile.id.as_str(), "exhausted_until": reset,
                    "backend_resets_at": reset, "quota_failure_at": chrono::Utc::now(),
                    "rate_limits": {"primary": {"used_percent": 100.0, "resets_at": reset,
                        "window_minutes": 300}, "observed_at": chrono::Utc::now()}
                }]
            }))?,
        )?;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "account_id": "account-123", "rate_limit": {"allowed": false,
                    "limit_reached": true}
            })))
            .mount(&server)
            .await;
        let store = std::sync::Arc::new(codex_login::AccountRuntimeStateStore::new(
            home.path().to_path_buf(),
        ));
        let writer = store.clone();
        let expected = std::sync::Arc::new(std::sync::Mutex::new(None));
        let saved = expected.clone();
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .respond_with(move |_: &wiremock::Request| {
                let mut state = writer.load().expect("synthetic shared quota");
                if new_refusal {
                    state.profiles[0].quota_failure_at = Some(chrono::Utc::now());
                    writer.save(&state).expect("concurrent persisted refusal");
                }
                *saved.lock().expect("expected profile") = Some(state.profiles[0].clone());
                ResponseTemplate::new(/*s*/ 200)
                    .set_delay(std::time::Duration::from_millis(/*millis*/ 100))
                    .set_body_json(json!({"code": code, "windows_reset": windows}))
            })
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let mut app = initialized_app_server(home.path()).await?;
        consume_reset_credit(&mut app, "synthetic-delayed-reset").await?;
        // A terminal replay is a receipt for the original operation, not new recovery evidence.
        consume_reset_credit(&mut app, "synthetic-delayed-reset").await?;
        assert_eq!(
            store.load()?.profiles[0],
            expected
                .lock()
                .expect("expected profile")
                .clone()
                .expect("POST occurred")
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_waits_for_the_cross_process_spending_lock() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let store = codex_login::AccountRuntimeStateStore::new(home.path().to_path_buf());
    let lock = store
        .try_lock_reset_credit()?
        .expect("hold spending lock in another process");
    let mut app = initialized_app_server(home.path()).await?;
    let id = send_consume_reset_credit(&mut app, "shared-spending-lock").await?;
    tokio::time::sleep(std::time::Duration::from_millis(/*millis*/ 100)).await;
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .all(|request| request.method != "POST")
    );
    drop(lock);
    let response: ConsumeAccountRateLimitResetCreditResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(id)).await??;
    assert_eq!(
        response,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    Ok(())
}

#[tokio::test]
async fn consume_revalidates_credit_scope_status_and_expiry_before_spending() -> Result<()> {
    for (scope, status, expires) in [
        ("unknown", "available", "2099-01-01T00:00:00Z"),
        ("codex_rate_limits", "redeemed", "2099-01-01T00:00:00Z"),
        ("codex_rate_limits", "available", "2000-01-01T00:00:00Z"),
    ] {
        let (home, server) = chatgpt_test_context().await?;
        Mock::given(method("GET"))
            .and(path("/api/codex/rate-limit-reset-credits"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "available_count": 1, "credits": [{"id": "credit-123", "reset_type": scope,
                    "status": status, "granted_at": "2026-01-01T00:00:00Z", "expires_at": expires}]
            })))
            .with_priority(/*p*/ 1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(/*s*/ 500))
            .expect(/*r*/ 0)
            .mount(&server)
            .await;
        let mut app = initialized_app_server(home.path()).await?;
        let id = app
            .send_consume_account_rate_limit_reset_credit_request(
                ConsumeAccountRateLimitResetCreditParams {
                    expected_owner_key: None,
                    idempotency_key: "invalid-credit".into(),
                    credit_id: Some("credit-123".into()),
                },
            )
            .await?;
        assert_eq!(
            read_error_response(&mut app, id).await?.error.code,
            INVALID_REQUEST_ERROR_CODE
        );
        assert_eq!(
            consume_reset_credit(&mut app, "no-eligible-credit").await?,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_replays_the_original_credit_after_a_lost_response_and_restart() -> Result<()> {
    for inventory_after_spending in ["redeemed", "missing", "expired"] {
        let (home, server) = chatgpt_test_context().await?;
        let primary_reset_at = chrono::Utc::now().timestamp() + 3600;
        let spent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_spent = spent.clone();
        Mock::given(method("GET"))
            .and(path("/api/codex/rate-limit-reset-credits"))
            .respond_with(move |_: &wiremock::Request| {
                let spent = observed_spent.load(std::sync::atomic::Ordering::SeqCst);
                let credits = if spent && inventory_after_spending == "missing" {
                    vec![]
                } else {
                    vec![json!({
                        "id": "credit-123", "reset_type": "codex_rate_limits",
                        "status": if spent && inventory_after_spending == "redeemed" {
                            "redeemed"
                        } else { "available" },
                        "granted_at": "2026-01-01T00:00:00Z",
                        "expires_at": if spent && inventory_after_spending == "expired" {
                            Some("2000-01-01T00:00:00Z")
                        } else { None }
                    })]
                };
                ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                    "available_count": if spent { 0 } else { 1 }, "credits": credits
                }))
            })
            .with_priority(/*p*/ 1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(body_json(json!({
                "redeem_request_id": "lost-response", "credit_id": "credit-123"
            })))
            .respond_with(move |_: &wiremock::Request| {
                if spent.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    ResponseTemplate::new(/*s*/ 200)
                        .set_body_json(json!({"code": "already_redeemed", "windows_reset": 0}))
                } else {
                    ResponseTemplate::new(/*s*/ 200)
                        .set_delay(std::time::Duration::from_secs(/*secs*/ 1))
                        .set_body_json(json!({"code": "reset", "windows_reset": 2}))
                }
            })
            .expect(/*r*/ 2)
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "account_id": "account-123", "user_id": "user-123", "plan_type": "pro",
                "rate_limit": {"allowed": true, "limit_reached": false,
                    "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                        "reset_after_seconds": 3600, "reset_at": primary_reset_at},
                    "secondary_window": {"used_percent": 0, "limit_window_seconds": 604800,
                        "reset_after_seconds": 7200, "reset_at": chrono::Utc::now().timestamp() + 7200}}
            }))).expect(/*r*/ 1).mount(&server).await;
        let mut app = TestAppServer::builder()
            .with_codex_home(home.path())
            .without_auto_env()
            .with_env_overrides(&[
                ("OPENAI_API_KEY", None),
                (RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR, Some("250")),
            ])
            .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
            .await?;
        let id = send_consume_reset_credit(&mut app, "lost-response").await?;
        assert_eq!(
            read_error_response(&mut app, id).await?.error.message,
            "rate limit reset consume timed out"
        );
        drop(app);

        let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(
            home.path().join(".manual-rate-limit-reset-credits.json"),
        )?)?;
        let original_owner = binding["operations"][0]["ownerDigest"]
            .as_str()
            .unwrap()
            .to_owned();
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new("another-seat-token")
                .account_id("account-123")
                .chatgpt_user_id("another-seat")
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
        let mut changed = initialized_app_server(home.path()).await?;
        let rejected = changed
            .send_raw_request(
                "account/rateLimitResetCredit/consume",
                Some(json!({
                    "idempotencyKey":"lost-response", "creditId":"credit-123",
                    "expectedOwnerKey":original_owner
                })),
            )
            .await?;
        assert_eq!(
            read_error_response(&mut changed, rejected)
                .await?
                .error
                .code,
            INVALID_REQUEST_ERROR_CODE
        );
        drop(changed);
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new("same-owner-refreshed-token")
                .account_id("account-123")
                .chatgpt_user_id("user-123")
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;

        let mut app = initialized_app_server(home.path()).await?;
        let id = app
            .send_consume_account_rate_limit_reset_credit_request(
                ConsumeAccountRateLimitResetCreditParams {
                    expected_owner_key: Some(original_owner),
                    idempotency_key: "lost-response".into(),
                    credit_id: Some("credit-123".into()),
                },
            )
            .await?;
        assert_eq!(
            timeout(
                DEFAULT_READ_TIMEOUT,
                app.read_response::<ConsumeAccountRateLimitResetCreditResponse>(id)
            )
            .await??,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
            }
        );
        let quota_id = app.send_get_account_rate_limits_request().await?;
        let quota = timeout(
            DEFAULT_READ_TIMEOUT,
            app.read_response::<GetAccountRateLimitsResponse>(quota_id),
        )
        .await??;
        assert_eq!(
            (quota.ordinary_usage_allowed, quota.rate_limits.primary),
            (
                Some(true),
                Some(RateLimitWindow {
                    used_percent: 0,
                    window_duration_mins: Some(300),
                    resets_at: Some(primary_reset_at)
                })
            )
        );
        assert_eq!(
            consume_reset_credit(&mut app, "new-operation").await?,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_rejects_a_known_key_with_a_changed_credit_or_owner() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("chatgpt-account-id", "account-123"))
        .and(body_json(
            json!({"redeem_request_id": "bound-operation", "credit_id": "credit-123"}),
        ))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    consume_reset_credit(&mut app, "bound-operation").await?;
    Mock::given(method("GET")).and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "available_count": 1, "credits": [{"id": "another-credit", "reset_type": "codex_rate_limits",
                "status": "available", "granted_at": "2026-01-01T00:00:00Z", "expires_at": null}]
        }))).with_priority(/*p*/ 1).mount(&server).await;
    let changed_credit = app
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                expected_owner_key: None,
                idempotency_key: "bound-operation".into(),
                credit_id: Some("another-credit".into()),
            },
        )
        .await?;
    let error = read_error_response(&mut app, changed_credit).await?;
    assert_eq!((error.error.code, error.error.message), (INVALID_REQUEST_ERROR_CODE,
        "idempotencyKey is bound to a different account or credit; retry the original operation".into()));
    drop(app);
    for (account, user) in [
        ("another-account", "user-123"),
        ("account-123", "another-user"),
    ] {
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new("another-synthetic-token")
                .account_id(account)
                .chatgpt_user_id(user)
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
        let mut app = initialized_app_server(home.path()).await?;
        let changed_owner = send_consume_reset_credit(&mut app, "bound-operation").await?;
        let error = read_error_response(&mut app, changed_owner).await?;
        assert_eq!((error.error.code, error.error.message), (INVALID_REQUEST_ERROR_CODE,
            "idempotencyKey is bound to a different account or credit; retry the original operation".into()));
    }
    Ok(())
}

#[tokio::test]
async fn consume_without_a_credit_id_uses_earliest_expiry_and_a_stable_id_tie_breaker() -> Result<()>
{
    for order in [[0, 1, 2, 3, 4], [4, 3, 2, 0, 1], [1, 0, 4, 3, 2]] {
        let (home, server) = chatgpt_test_context().await?;
        let credits = [
            ("no-expiry", None),
            ("later", Some("2099-02-01T00:00:00Z")),
            ("b-first", Some("2099-01-01T00:00:00.100Z")),
            ("a-first", Some("2098-12-31T19:00:00.100-05:00")),
            ("0-later-fraction", Some("2099-01-01T00:00:00.900Z")),
        ];
        let credits: Vec<_> = order
            .into_iter()
            .map(|index| {
                let (id, expires) = credits[index];
                json!({"id": id, "reset_type": "codex_rate_limits", "status": "available",
                "granted_at": "2026-01-01T00:00:00Z", "expires_at": expires})
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/api/codex/rate-limit-reset-credits"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "available_count": 5, "credits": credits
            })))
            .with_priority(/*p*/ 1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(body_json(
                json!({"redeem_request_id": "earliest-expiry", "credit_id": "a-first"}),
            ))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200)
                    .set_body_json(json!({"code": "reset", "windows_reset": 2})),
            )
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let mut app = initialized_app_server(home.path()).await?;
        assert_eq!(
            consume_reset_credit(&mut app, "earliest-expiry").await?,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_rejects_oversized_request_bindings_before_spending() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    for params in [
        ConsumeAccountRateLimitResetCreditParams {
            expected_owner_key: None,
            idempotency_key: "x".repeat(129),
            credit_id: None,
        },
        ConsumeAccountRateLimitResetCreditParams {
            expected_owner_key: None,
            idempotency_key: "request".into(),
            credit_id: Some("x".repeat(257)),
        },
        ConsumeAccountRateLimitResetCreditParams {
            expected_owner_key: Some("not-a-reset-owner".into()),
            idempotency_key: "request".into(),
            credit_id: None,
        },
    ] {
        let id = app
            .send_consume_account_rate_limit_reset_credit_request(params)
            .await?;
        assert_eq!(
            read_error_response(&mut app, id).await?.error.code,
            INVALID_REQUEST_ERROR_CODE
        );
    }
    Ok(())
}

#[tokio::test]
async fn consume_refuses_to_post_when_the_binding_cannot_be_persisted() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    let journal_path = home.path().join(".manual-rate-limit-reset-credits.json");
    Mock::given(method("GET")).and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            std::fs::create_dir(&journal_path).expect("block synthetic journal persistence");
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "available_count": 1, "credits": [{"id": "credit-123", "reset_type": "codex_rate_limits",
                    "status": "available", "granted_at": "2026-01-01T00:00:00Z", "expires_at": null}]
            }))
        }).with_priority(/*p*/ 1).mount(&server).await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    let id = send_consume_reset_credit(&mut app, "persist-before-send").await?;
    let error = read_error_response(&mut app, id).await?;
    assert_eq!(
        (error.error.code, error.error.message),
        (
            INTERNAL_ERROR_CODE,
            "reset operation journal is unavailable; no credit was spent".into()
        )
    );
    Ok(())
}

#[tokio::test]
async fn consume_rejects_confirmation_for_a_previous_workspace_or_business_seat() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "account_id": "account-123", "user_id": "user-123", "plan_type": "business",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 43, "limit_window_seconds": 18000,
                    "reset_after_seconds": 3600, "reset_at": 2000000000}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "code": "reset", "windows_reset": 2
        })))
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    let read = app.send_get_account_rate_limits_request().await?;
    let response: GetAccountRateLimitsResponse = app.read_response(read).await?;
    let owner = serde_json::to_value(response)?["resetOwnerKey"]
        .as_str()
        .expect("the same usage read binds its reset owner")
        .to_owned();
    drop(app);
    for (workspace, user) in [
        ("another-workspace", "user-123"),
        ("account-123", "another-business-seat"),
    ] {
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new("changed-synthetic-token")
                .account_id(workspace)
                .chatgpt_user_id(user)
                .plan_type("business"),
            AuthCredentialsStoreMode::File,
        )?;
        let mut app = initialized_app_server(home.path()).await?;
        let consume = app
            .send_raw_request(
                "account/rateLimitResetCredit/consume",
                Some(json!({"idempotencyKey": "old-confirmation", "expectedOwnerKey": owner})),
            )
            .await?;
        let error = read_error_response(&mut app, consume).await?;
        assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
        assert!(error.error.message.contains("account changed"));
    }
    Ok(())
}

#[tokio::test]
async fn an_unconfirmed_reset_blocks_a_new_key_until_the_original_operation_completes() -> Result<()>
{
    let (home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(body_json(
            json!({"redeem_request_id": "unconfirmed", "credit_id": "credit-123"}),
        ))
        .respond_with(ResponseTemplate::new(/*s*/ 503))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    let original = send_consume_reset_credit(&mut app, "unconfirmed").await?;
    assert_eq!(
        read_error_response(&mut app, original).await?.error.code,
        INTERNAL_ERROR_CODE
    );
    drop(app);
    let mut app = initialized_app_server(home.path()).await?;
    let replacement = send_consume_reset_credit(&mut app, "replacement-key").await?;
    let error = read_error_response(&mut app, replacement).await?;
    assert!(error.error.message.contains("original operation"));
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn root_owner_change_during_credit_validation_prevents_spending() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    let credential_home = home.path().to_path_buf();
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            write_chatgpt_auth(
                &credential_home,
                ChatGptAuthFixture::new("changed-root-token")
                    .account_id("account-123")
                    .chatgpt_user_id("another-seat")
                    .plan_type("business"),
                AuthCredentialsStoreMode::File,
            )
            .expect("change synthetic root owner");
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "available_count":1, "credits":[{"id":"credit-123",
                    "reset_type":"codex_rate_limits", "status":"available",
                    "granted_at":"2026-01-01T00:00:00Z", "expires_at":null}]
            }))
        })
        .with_priority(/*p*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(/*s*/ 200))
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let mut app = initialized_app_server(home.path()).await?;
    let consume = send_consume_reset_credit(&mut app, "root-owner-changed").await?;
    let error = read_error_response(&mut app, consume).await?;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(error.error.message.contains("account changed before reset"));
    assert!(
        !home
            .path()
            .join(".manual-rate-limit-reset-credits.json")
            .exists()
    );
    Ok(())
}

async fn chatgpt_test_context() -> Result<(TempDir, MockServer)> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .chatgpt_user_id("user-123")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "available_count": 1, "credits": [{"id": "credit-123", "reset_type": "codex_rate_limits",
                "status": "available", "granted_at": "2026-01-01T00:00:00Z", "expires_at": null}]
        }))).with_priority(/*p*/ 10).mount(&server).await;
    write_chatgpt_base_url(codex_home.path(), &server.uri())?;
    Ok((codex_home, server))
}

async fn initialized_app_server(codex_home: &Path) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(codex_home)
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await
}

async fn consume_reset_credit(
    mcp: &mut TestAppServer,
    idempotency_key: &str,
) -> Result<ConsumeAccountRateLimitResetCreditResponse> {
    let request_id = send_consume_reset_credit(mcp, idempotency_key).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await?
}

async fn send_consume_reset_credit(mcp: &mut TestAppServer, idempotency_key: &str) -> Result<i64> {
    mcp.send_consume_account_rate_limit_reset_credit_request(
        ConsumeAccountRateLimitResetCreditParams {
            expected_owner_key: None,
            idempotency_key: idempotency_key.to_string(),
            credit_id: None,
        },
    )
    .await
}

async fn read_error_response(mcp: &mut TestAppServer, request_id: i64) -> Result<JSONRPCError> {
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    Ok(error)
}

async fn login_with_api_key(mcp: &mut TestAppServer, api_key: &str) -> Result<()> {
    let request_id = mcp.send_login_account_api_key_request(api_key).await?;
    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<LoginAccountResponse>(request_id),
        )
        .await??,
        LoginAccountResponse::ApiKey {}
    );
    Ok(())
}

fn write_chatgpt_base_url(codex_home: &Path, base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("chatgpt_base_url = \"{base_url}\"\n"),
    )
}

#[tokio::test]
async fn unconfirmed_automatic_reset_blocks_a_new_manual_post() -> Result<()> {
    let (home, server) = chatgpt_test_context().await?;
    std::fs::write(
        home.path()
            .join(format!(".rate-limit-reset-credit-{}.json", "a".repeat(40))),
        serde_json::to_vec(
            &json!({"version":1,"scope":{"profileId":"synthetic-profile", "ownerKey":"synthetic-owner"},
            "attemptedAt":1,"requestId":"original-auto-request","phase":{"state":"pending"}}),
        )?,
    )?;
    let mut app = initialized_app_server(home.path()).await?;
    let request_id = send_consume_reset_credit(&mut app, "new-manual-request").await?;
    let error = read_error_response(&mut app, request_id).await?;
    assert!(
        error
            .error
            .message
            .contains("automatic reset is unconfirmed"),
        "{error:?}"
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .all(|request| request.method.as_str() != "POST")
    );
    Ok(())
}
