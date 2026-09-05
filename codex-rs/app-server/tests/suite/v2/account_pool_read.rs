use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use base64::Engine as _;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetAuthStatusParams;
use codex_app_server_protocol::GetAuthStatusResponse;
use codex_protocol::account::PlanType as AccountPlanType;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn create_config_toml(codex_home: &Path, chatgpt_base_url: Option<&str>) -> std::io::Result<()> {
    let chatgpt_base_url_line = chatgpt_base_url
        .map(|url| format!("chatgpt_base_url = \"{url}\"\n"))
        .unwrap_or_default();
    // Keep stock OpenAI provider defaults; only override the ChatGPT API base when mocking.
    std::fs::write(codex_home.join("config.toml"), chatgpt_base_url_line)
}

fn write_profile_credentials(codex_home: &Path, id: &str, access_token: &str) {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64(&serde_json::to_vec(&json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": format!("account-{id}"),
        }
    }))
    .expect("payload"));
    let fake_jwt = format!("{header}.{payload}.{}", b64(b"sig"));

    let credential_home = codex_home.join("auth-profiles").join(id);
    std::fs::create_dir_all(&credential_home).expect("credential home");
    std::fs::write(
        credential_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": access_token,
                "refresh_token": format!("refresh-{id}"),
                "account_id": format!("account-{id}"),
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .expect("auth.json"),
    )
    .expect("write auth.json");
}

/// Pool-only home: profile credentials exist, but root CODEX_HOME has no auth.json.
fn write_pool_only_fixture(codex_home: &Path) {
    write_profile_credentials(codex_home, "selected-acct", "access-selected");
    std::fs::write(
        codex_home.join("account-profiles.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "profiles": [{
                "id": "selected-acct",
                "label": null,
                "priority": 0,
                "credential_location": "managed_profile",
                "state": "ready",
                "disabled": false,
            }],
        }))
        .expect("manifest"),
    )
    .expect("write manifest");
    std::fs::write(
        codex_home.join("account-runtime-state.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "active_profile_id": "selected-acct",
            "profiles": [],
        }))
        .expect("runtime state"),
    )
    .expect("write runtime state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_read_reports_chatgpt_when_only_pool_profile_has_credentials() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), /*chatgpt_base_url*/ None)?;
    write_models_cache(codex_home.path())?;
    write_pool_only_fixture(codex_home.path());

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let get_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let account: GetAccountResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(get_id)).await??;

    assert_eq!(
        account.account,
        Some(Account::Chatgpt {
            email: Some("selected-acct@example.com".to_string()),
            plan_type: AccountPlanType::Pro,
        })
    );
    let pool = account.account_pool.expect("account pool snapshot");
    assert!(pool.enabled);
    assert_eq!(pool.active_profile_id.as_deref(), Some("selected-acct"));

    let status_id = mcp
        .send_get_auth_status_request(GetAuthStatusParams {
            include_token: Some(false),
            refresh_token: Some(false),
        })
        .await?;
    let status: GetAuthStatusResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(status_id)).await??;
    assert_eq!(
        status.auth_method,
        Some(codex_app_server_protocol::AuthMode::Chatgpt)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limits_use_pool_profile_credentials_without_root_auth() -> Result<()> {
    use codex_app_server_protocol::GetAccountRateLimitsResponse;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    create_config_toml(codex_home.path(), Some(&server.uri()))?;
    write_models_cache(codex_home.path())?;
    write_pool_only_fixture(codex_home.path());

    let reset_at = chrono::Utc::now().timestamp() + 3600;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-selected"))
        .and(header("chatgpt-account-id", "account-selected-acct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "account-selected-acct",
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 77,
                    "limit_window_seconds": 3600,
                    "reset_after_seconds": 3600,
                    "reset_at": reset_at,
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "credits": [],
            "available_count": 0,
            "total_earned_count": 0
        })))
        .mount(&server)
        .await;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let request_id = mcp.send_get_account_rate_limits_request().await?;
    let received: GetAccountRateLimitsResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await??;

    assert_eq!(
        received.account_id.as_deref(),
        Some("account-selected-acct")
    );
    assert_eq!(
        received
            .rate_limits
            .primary
            .as_ref()
            .map(|window| window.used_percent),
        Some(77)
    );

    Ok(())
}
