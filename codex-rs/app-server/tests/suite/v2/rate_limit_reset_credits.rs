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
