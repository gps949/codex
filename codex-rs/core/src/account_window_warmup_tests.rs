//! Warmup model/effort selection lives in `codex-models-manager::warmup_selection`.
//! Keep a thin smoke test here so core still exercises the catalog-driven path.
//! Also cover escalating failure backoff and success-path helpers that prevent false NOOP retries.

use super::*;
use crate::config::ConfigBuilder;
use codex_features::Feature;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
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
    let model = codex_models_manager::select_cheapest_warmup_model(&catalog)
        .expect("bundled catalog should expose a cheapest warmup model");
    let effort = codex_models_manager::cheapest_supported_effort(&model)
        .expect("selected warmup model should advertise at least one effort");

    assert!(
        !model.slug.is_empty(),
        "warmup model slug must come from the official catalog"
    );
    assert_ne!(
        effort,
        codex_protocol::openai_models::ReasoningEffort::Minimal,
        "bundled cheapest model currently rejects unsupported minimal effort"
    );
}

#[test]
fn backoff_for_streak_escalates_hard_and_noop() {
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 1),
        Duration::from_secs(5 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 2),
        Duration::from_secs(15 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 3),
        Duration::from_secs(45 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 4),
        Duration::from_secs(3 * 60 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Hard, 5),
        MAX_FAILURE_BACKOFF
    );

    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 1),
        Duration::from_secs(30 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 2),
        Duration::from_secs(2 * 60 * 60)
    );
    assert_eq!(
        backoff_for_streak(FailureKind::Noop, 3),
        MAX_FAILURE_BACKOFF
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
    _codex_home: TempDir,
}

async fn warmup_request_fixture(
    enable_agent_identity: bool,
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

    let sse_body = sse(vec![
        ev_response_created("resp-warmup"),
        ev_completed("resp-warmup"),
    ]);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-primary-used-percent", "1.0")
                .insert_header("x-codex-primary-window-minutes", "300")
                .set_body_raw(sse_body, "text/event-stream"),
        )
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
        _codex_home: codex_home,
    })
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

fn warmup_outcome(pool: &AccountPool, profile_id: &AccountProfileId) -> WindowWarmupOutcome {
    pool.snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.window_warmup)
        .map(|observation| observation.outcome)
        .expect("warmup observation")
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

    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Failed
    );
    assert!(fixture.register_count.load(Ordering::SeqCst) >= 1);
    Ok(())
}
