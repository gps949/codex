//! Warmup model/effort selection lives in `codex-models-manager::warmup_selection`.
//! Keep a thin smoke test here so core still exercises the catalog-driven path.
//! Also cover success-path helpers that prevent false NOOP retries.

use super::*;
use crate::config::ConfigBuilder;
use codex_features::Feature;
use codex_login::AccountProfile;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::WindowWarmupOutcome;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_models_once;
use core_test_support::responses::sse;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const TEST_CHATGPT_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";

fn rate_limit_window(used_percent: f64) -> RateLimitWindow {
    RateLimitWindow {
        used_percent,
        window_minutes: Some(300),
        resets_at: None,
    }
}

fn rate_limit_snapshot(limit_id: Option<&str>, used_percent: f64) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: limit_id.map(str::to_string),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(rate_limit_window(used_percent)),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

#[test]
fn catalog_driven_warmup_selection_is_available_to_core() {
    let catalog = codex_models_manager::warmup_models_catalog(/*preferred*/ None);
    let model = codex_models_manager::select_warmup_model(&catalog, /*preferred_slug*/ None)
        .expect("bundled catalog should expose a default warmup model");
    let effort = codex_models_manager::warmup_supported_effort(&model, /*preferred*/ None)
        .expect("selected warmup model should advertise at least one effort");

    assert!(
        !model.slug.is_empty(),
        "warmup model slug must come from the official catalog"
    );
    assert_ne!(model.slug, "gpt-5.2");
    assert_ne!(model.slug, "gpt-5.5");
    assert!(!model.slug.contains("luna"));
    assert_ne!(
        effort,
        codex_protocol::openai_models::ReasoningEffort::Minimal,
        "bundled default model currently rejects unsupported minimal effort"
    );
}

#[test]
fn prefer_rate_limit_snapshot_prefers_codex_and_higher_usage() {
    let other = rate_limit_snapshot(Some("codex_other"), 12.0);
    let codex_idle = rate_limit_snapshot(Some("codex"), 0.0);
    let preferred = prefer_rate_limit_snapshot(Some(other), codex_idle.clone());
    assert_eq!(preferred.limit_id.as_deref(), Some("codex"));

    let codex_started = rate_limit_snapshot(Some("codex"), 3.0);
    let preferred = prefer_rate_limit_snapshot(Some(codex_idle), codex_started);
    assert_eq!(
        preferred.primary.as_ref().map(|window| window.used_percent),
        Some(3.0)
    );
}

#[test]
fn merge_account_rate_limits_monotonic_keeps_started_primary() {
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 2.5,
            resets_at: Some(Utc::now() + chrono::Duration::hours(4)),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(Utc::now() - chrono::Duration::seconds(30)),
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: None,
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(Utc::now()),
    };
    let merged = merge_account_rate_limits_monotonic(Some(&existing), incoming);
    assert_eq!(
        merged.primary.as_ref().map(|window| window.used_percent),
        Some(2.5)
    );
}

struct WarmupRequestFixture {
    pool: AccountPool,
    profile_id: AccountProfileId,
    auth_manager: Arc<AuthManager>,
    config: Config,
    register_count: Arc<AtomicUsize>,
    server: MockServer,
    _codex_home: TempDir,
}

async fn warmup_request_fixture(
    enable_agent_identity: bool,
) -> anyhow::Result<WarmupRequestFixture> {
    warmup_request_fixture_with_primary_used_percent(
        enable_agent_identity,
        /*primary_used_percent*/ Some("1.0"),
    )
    .await
}

async fn warmup_request_fixture_idle_stream(
    enable_agent_identity: bool,
) -> anyhow::Result<WarmupRequestFixture> {
    warmup_request_fixture_with_primary_used_percent(
        enable_agent_identity,
        /*primary_used_percent*/ None,
    )
    .await
}

async fn warmup_request_fixture_with_primary_used_percent(
    enable_agent_identity: bool,
    primary_used_percent: Option<&'static str>,
) -> anyhow::Result<WarmupRequestFixture> {
    warmup_request_fixture_with_sse(
        enable_agent_identity,
        primary_used_percent,
        sse(vec![
            ev_response_created("resp-warmup"),
            ev_completed("resp-warmup"),
        ]),
        /*first_unusable_model_message*/ None,
    )
    .await
}

async fn warmup_request_fixture_with_sse(
    enable_agent_identity: bool,
    primary_used_percent: Option<&'static str>,
    sse_body: String,
    first_unusable_model_message: Option<&'static str>,
) -> anyhow::Result<WarmupRequestFixture> {
    let server = MockServer::start().await;
    let register_count = Arc::new(AtomicUsize::new(0));
    let register_hits = Arc::clone(&register_count);
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(move |_request: &wiremock::Request| {
            register_hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(/*status*/ 403)
        })
        .mount(&server)
        .await;

    let mut responses = ResponseTemplate::new(/*status*/ 200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse_body, "text/event-stream");
    if let Some(used_percent) = primary_used_percent {
        responses = responses
            .insert_header("x-codex-primary-used-percent", used_percent)
            .insert_header("x-codex-primary-window-minutes", "300");
    }
    let response_hits = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_request: &wiremock::Request| {
            let attempt = response_hits.fetch_add(1, Ordering::SeqCst);
            if attempt == 0
                && let Some(message) = first_unusable_model_message
            {
                return ResponseTemplate::new(/*status*/ 400).set_body_json(serde_json::json!({
                    "error": {
                        "type": "invalid_request_error",
                        "message": message,
                    }
                }));
            }
            responses.clone()
        })
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    write_chatgpt_auth_json(codex_home.path());
    let loaded = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await;
    let auth = loaded.auth().await.expect("chatgpt auth should load");
    let auth_manager = AuthManager::from_auth_for_testing_with_agent_identity_authapi_base_url(
        auth,
        codex_home.path().to_path_buf(),
        server.uri(),
    );

    let profile_id = AccountProfileId::new("standby").expect("valid profile id");
    let pool = AccountPool::new();
    pool.register(
        AccountProfile::new(
            profile_id.clone(),
            codex_home.path().to_path_buf(),
            /*priority*/ 10,
            Some("standby".to_string()),
        ),
        Arc::clone(&auth_manager),
    )?;

    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("load test config");
    config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider.supports_websockets = false;
    config.chatgpt_base_url = server.uri();
    if enable_agent_identity {
        config
            .features
            .enable(Feature::UseAgentIdentity)
            .expect("enable use_agent_identity");
    }

    Ok(WarmupRequestFixture {
        pool,
        profile_id,
        auth_manager,
        config,
        register_count,
        server,
        _codex_home: codex_home,
    })
}

fn live_warmup_catalog(slug: &str) -> codex_protocol::openai_models::ModelsResponse {
    let bundled = codex_models_manager::bundled_models_response().expect("bundled catalog");
    let mut model = bundled
        .models
        .iter()
        .find(|model| model.slug == "gpt-6-astra")
        .cloned()
        .expect("bundled gpt-6-astra");
    model.slug = slug.to_string();
    model.priority = 0;
    codex_protocol::openai_models::ModelsResponse {
        models: vec![model],
    }
}

fn write_chatgpt_auth_json(codex_home: &std::path::Path) {
    let auth_json = serde_json::json!({
        "tokens": {
            "id_token": TEST_CHATGPT_ID_TOKEN,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "account_id": "account-123"
        },
        "last_refresh": "2099-01-01T00:00:00Z"
    });
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::to_string_pretty(&auth_json).expect("serialize auth.json"),
    )
    .expect("write auth.json");
}

fn warmup_outcome_opt(
    pool: &AccountPool,
    profile_id: &AccountProfileId,
) -> Option<WindowWarmupOutcome> {
    pool.snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.window_warmup)
        .map(|observation| observation.outcome)
}

fn warmup_outcome(pool: &AccountPool, profile_id: &AccountProfileId) -> WindowWarmupOutcome {
    warmup_outcome_opt(pool, profile_id).expect("warmup observation")
}

#[tokio::test]
async fn warmup_sends_request_without_agent_identity_when_feature_is_off() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Succeeded
    );
    assert_eq!(fixture.register_count.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn warmup_still_registers_agent_identity_when_feature_is_on() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ true).await?;

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    assert_eq!(warmup_outcome_opt(&fixture.pool, &fixture.profile_id), None);
    assert!(fixture.register_count.load(Ordering::SeqCst) >= 1);
    Ok(())
}

#[tokio::test]
async fn warmup_posts_chatgpt_capable_model_with_process_originator() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    let requests = fixture.server.received_requests().await.expect("requests");
    let warmup = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("warmup responses POST");
    let body: serde_json::Value = serde_json::from_slice(&warmup.body)?;
    let model = body["model"].as_str().expect("model");
    assert_ne!(model, "gpt-5.6-luna");
    assert_ne!(model, "gpt-5.2");
    assert_ne!(model, "gpt-5.5");
    assert!(
        !model.contains("luna"),
        "warmup must not send a Luna/reserve model: {model}"
    );
    let originator = warmup
        .headers
        .get("originator")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(codex_login::default_client::DEFAULT_ORIGINATOR);
    assert!(
        codex_login::default_client::is_first_party_originator(originator),
        "warmup originator must be first-party, got {originator}"
    );
    let turn_metadata: serde_json::Value = warmup
        .headers
        .get("x-codex-turn-metadata")
        .and_then(|value| value.to_str().ok())
        .and_then(|json| serde_json::from_str(json).ok())
        .expect("turn metadata");
    let installation_id = turn_metadata["installation_id"]
        .as_str()
        .expect("installation_id");
    assert_ne!(installation_id, "account-window-warmup");
    assert!(
        uuid::Uuid::parse_str(installation_id).is_ok(),
        "warmup installation id must be a UUID, got {installation_id}"
    );
    let instructions = body["instructions"].as_str().unwrap_or("");
    let input_text_len = body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .map(str::len)
        .sum::<usize>();
    assert_ne!(instructions, "Reply with one short token.");
    assert!(
        instructions.len() > 200 || input_text_len > 200,
        "warmup must send real Codex instructions, got {} top-level chars and {input_text_len} input chars",
        instructions.len()
    );
    if let Some(tools) = body["tools"].as_array() {
        assert!(
            !tools.is_empty(),
            "warmup must send the default Codex tool harness"
        );
        assert!(
            tools.iter().any(|tool| {
                matches!(
                    tool["name"].as_str(),
                    Some("exec_command" | "apply_patch" | "update_plan")
                )
            }),
            "warmup tools must include a real Codex tool, got {tools:?}"
        );
    } else {
        let input = body["input"].as_array().expect("lite input");
        assert!(
            input.iter().any(|item| item["type"] == "additional_tools"
                && item["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty())),
            "lite warmup must still send tools in input, got {input:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn warmup_uses_official_models_endpoint_catalog() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let live_slug = "live-catalog-default";
    mount_models_once(&fixture.server, live_warmup_catalog(live_slug)).await;

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    let requests = fixture.server.received_requests().await.expect("requests");
    assert!(
        requests
            .iter()
            .any(|request| request.url.path().ends_with("/models")),
        "warmup must list models via Codex GET /models, got {:?}",
        requests
            .iter()
            .map(|request| request.url.path().to_string())
            .collect::<Vec<_>>()
    );
    let warmup = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("warmup responses POST");
    let body: serde_json::Value = serde_json::from_slice(&warmup.body)?;
    assert_eq!(body["model"].as_str(), Some(live_slug));
    Ok(())
}

#[tokio::test]
async fn warmup_posts_session_model_when_chatgpt_capable() -> anyhow::Result<()> {
    let mut fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    fixture.config.model = Some("gpt-5.6-sol".to_string());

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    let requests = fixture.server.received_requests().await.expect("requests");
    let warmup = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("warmup responses POST");
    let body: serde_json::Value = serde_json::from_slice(&warmup.body)?;
    assert_eq!(body["model"].as_str(), Some("gpt-5.6-sol"));
    Ok(())
}

#[tokio::test]
async fn warmup_does_not_post_unknown_session_slug() -> anyhow::Result<()> {
    let mut fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    fixture.config.model = Some("does-not-exist".to_string());

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    let requests = fixture.server.received_requests().await.expect("requests");
    let warmup = requests
        .iter()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("warmup responses POST");
    let body: serde_json::Value = serde_json::from_slice(&warmup.body)?;
    let model = body["model"].as_str().expect("model");
    assert_ne!(model, "does-not-exist");
    assert_ne!(model, "gpt-5.2");
    assert_ne!(model, "gpt-5.5");
    assert!(!model.contains("luna"));
    Ok(())
}

#[tokio::test]
async fn warmup_retries_catalog_default_after_unusable_model_error() -> anyhow::Result<()> {
    let mut fixture = warmup_request_fixture_with_sse(
        /*enable_agent_identity*/ false,
        /*primary_used_percent*/ Some("1.0"),
        sse(vec![
            ev_response_created("resp-warmup"),
            ev_completed("resp-warmup"),
        ]),
        Some("The 'gpt-5.6-sol' model is not supported when using Codex with a ChatGPT account."),
    )
    .await?;
    fixture.config.model = Some("gpt-5.6-sol".to_string());

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    let requests = fixture.server.received_requests().await.expect("requests");
    let models: Vec<String> = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("body");
            body["model"].as_str().expect("model").to_string()
        })
        .collect();
    assert_eq!(
        models,
        vec!["gpt-5.6-sol".to_string(), "gpt-6-astra".to_string()]
    );
    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Succeeded
    );
    Ok(())
}

#[tokio::test]
async fn warmup_get_verifies_after_profile_becomes_active() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture_idle_stream(/*enable_agent_identity*/ false).await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200).set_body_json(serde_json::json!({
                "plan_type": "plus",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 2,
                        "limit_window_seconds": 18000,
                        "reset_after_seconds": 17000,
                        "reset_at": 2_000_000_000
                    }
                }
            })),
        )
        .mount(&fixture.server)
        .await;

    // earliest-reset can activate the profile while the POST is in flight. By the
    // post-stream check the target is current; GET must still prove the 5h start.
    let _lease = fixture.pool.lease().expect("activate warmed profile");

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Succeeded
    );
    let used = fixture
        .pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id == fixture.profile_id)
        .and_then(|snapshot| snapshot.rate_limits.primary)
        .map(|window| window.used_percent);
    assert_eq!(used, Some(2.0));
    Ok(())
}

#[tokio::test]
async fn warmup_get_verifies_after_tool_call_without_completed() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture_with_sse(
        /*enable_agent_identity*/ false,
        /*primary_used_percent*/ None,
        sse(vec![
            ev_response_created("resp-warmup"),
            ev_function_call("call-warmup", "exec_command", r#"{"cmd":"true"}"#),
        ]),
        /*first_unusable_model_message*/ None,
    )
    .await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200).set_body_json(serde_json::json!({
                "plan_type": "plus",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": {
                        "used_percent": 2,
                        "limit_window_seconds": 18000,
                        "reset_after_seconds": 17000,
                        "reset_at": 2_000_000_000
                    }
                }
            })),
        )
        .mount(&fixture.server)
        .await;

    warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        fixture.auth_manager,
    )
    .await?;

    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Succeeded
    );
    Ok(())
}
