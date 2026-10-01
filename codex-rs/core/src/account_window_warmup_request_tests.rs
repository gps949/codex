use super::*;
use crate::config::ConfigBuilder;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::SessionSource;
use core_test_support::responses::WebSocketConnectionConfig;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server_with_headers;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

struct WarmupStreamObservation {
    result: anyhow::Result<Option<RateLimitSnapshot>>,
    accepted: bool,
    limits: Option<RateLimitSnapshot>,
}

async fn run_warmup_websocket(events: Vec<Value>) -> anyhow::Result<WarmupStreamObservation> {
    let server = start_websocket_server_with_headers(vec![WebSocketConnectionConfig {
        requests: vec![events],
        response_headers: vec![
            ("OpenAI-Model".to_string(), "gpt-5.4".to_string()),
            ("X-Reasoning-Included".to_string(), "true".to_string()),
        ],
        accept_delay: None,
        close_after_requests: true,
    }])
    .await;
    let codex_home = TempDir::new()?;
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    let model_info = crate::test_support::construct_model_info_offline("gpt-5.4", &config);
    let mut provider = config.model_provider.clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.requires_openai_auth = false;
    provider.supports_websockets = true;
    provider.request_max_retries = Some(0);
    provider.stream_max_retries = Some(0);
    provider.stream_idle_timeout_ms = Some(2_000);
    let thread_id = ThreadId::new();
    let client = ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        thread_id,
        provider,
        SessionSource::Cli,
        "warmup-test".to_string(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*reasoning_effort_override_enabled*/ false,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        config.http_client_factory(),
        config.workspace_routing_context(),
        Vec::new(),
    );
    let telemetry = SessionTelemetry::new(
        thread_id,
        &model_info.slug,
        &model_info.slug,
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "warmup-test".to_string(),
        /*log_user_prompts*/ false,
        "warmup-test".to_string(),
        SessionSource::Cli,
    );
    let metadata = warmup_responses_metadata(uuid::Uuid::new_v4().to_string(), thread_id);
    let observed_limits = Arc::new(tokio::sync::Mutex::default());
    let accepted = AtomicBool::default();
    let result = stream_warmup_turn(
        &client,
        &warmup_prompt(&model_info),
        &model_info,
        &telemetry,
        /*effort*/ None,
        &metadata,
        WarmupStreamEvidence {
            observed_limits: &observed_limits,
            accepted: &accepted,
        },
    )
    .await;
    let observation = WarmupStreamObservation {
        result,
        accepted: accepted.load(Ordering::Relaxed),
        limits: observed_limits.lock().await.clone(),
    };
    server.shutdown().await;
    Ok(observation)
}

fn model_rejection() -> Value {
    json!({
        "type": "error",
        "status": 400,
        "error": { "type": "invalid_request_error", "message": "unknown model" },
    })
}

fn is_invalid_request(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<CodexErr>()
        .is_some_and(|error| matches!(error.details(), CodexErrorDetails::InvalidRequest(_)))
}

#[tokio::test]
async fn websocket_model_rejection_before_created_remains_unaccepted() -> anyhow::Result<()> {
    let observation = run_warmup_websocket(vec![model_rejection()]).await?;
    let error = observation.result.expect_err("model rejection");
    assert_eq!(
        (
            observation.accepted,
            is_invalid_request(&error),
            observation.limits,
        ),
        (false, true, None),
    );
    assert!(codex_models_manager::is_unusable_warmup_model_error(
        &error.to_string()
    ));
    Ok(())
}

#[tokio::test]
async fn websocket_quota_metadata_before_rejection_remains_unaccepted() -> anyhow::Result<()> {
    let observation = run_warmup_websocket(vec![
        json!({ "type": "codex.response.metadata", "headers": { "x-models-etag": "catalog" } }),
        json!({
            "type": "codex.rate_limits",
            "rate_limits": { "primary": { "used_percent": 0.0, "window_minutes": 300 } },
        }),
        model_rejection(),
    ])
    .await?;
    let error = observation.result.expect_err("model rejection");
    assert_eq!(
        (observation.accepted, observation.limits),
        (
            false,
            Some(RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                normal_model_slug: None,
                primary: Some(RateLimitWindow {
                    used_percent: 0.0,
                    window_minutes: Some(300),
                    resets_at: None,
                }),
                secondary: None,
                credits: None,
                individual_limit: None,
                spend_control_reached: None,
                plan_type: None,
                rate_limit_reached_type: None,
            })
        ),
    );
    assert!(
        is_invalid_request(&error),
        "unexpected rejection error: {error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn websocket_generation_evidence_before_rejection_preserves_acceptance() -> anyhow::Result<()>
{
    for event in [
        ev_response_created("resp-warmup"),
        json!({ "type": "response.output_text.delta", "delta": "2" }),
        json!({ "type": "response.custom_tool_call_input.delta", "item_id": "tool-1", "delta": "input" }),
    ] {
        let observation = run_warmup_websocket(vec![event, model_rejection()]).await?;
        let error = observation
            .result
            .expect_err("model rejection after generation evidence");
        assert_eq!(
            (observation.accepted, is_invalid_request(&error)),
            (true, true),
        );
    }
    Ok(())
}

#[tokio::test]
async fn websocket_completion_without_created_confirms_acceptance() -> anyhow::Result<()> {
    let observation = run_warmup_websocket(vec![ev_completed("resp-warmup")]).await?;
    assert_eq!(
        (
            observation.accepted,
            observation.result?,
            observation.limits
        ),
        (true, None, None)
    );
    Ok(())
}

#[test_case::test_case(false; "response_failed")]
#[test_case::test_case(true; "wrapped_error")]
#[tokio::test]
async fn websocket_model_quota_failure_keeps_metadata_scope_for_account_failover(
    wrapped: bool,
) -> anyhow::Result<()> {
    let quota = json!({"type":"usage_limit_reached", "message":"model allocation reached",
        "resets_at": chrono::Utc::now().timestamp() + 3600});
    let failure = if wrapped {
        json!({"type":"error", "status":429, "headers":{"x-request-id":"frame-request"}, "error":quota})
    } else {
        json!({"type":"response.failed", "response":{"error":quota}})
    };
    let observation = run_warmup_websocket(vec![
        json!({"type":"codex.response.metadata", "headers": {
            "x-codex-active-limit":"fast-model", "x-fast-model-limit-name":"gpt-fast",
            "x-fast-model-primary-used-percent":"100",
        }}),
        failure,
    ])
    .await?;
    let error = observation.result.expect_err("model quota refusal");
    let quota = error
        .downcast_ref::<CodexErr>()
        .expect("typed quota failure");
    let codex_protocol::error::CodexErrorDetails::UsageLimitReached(limit) = quota.details() else {
        panic!("expected subscription quota failure: {quota}");
    };
    assert_eq!(
        (
            observation.accepted,
            limit
                .rate_limits
                .as_ref()
                .and_then(|snapshot| snapshot.limit_name.as_deref())
        ),
        (false, Some("gpt-fast"))
    );
    Ok(())
}
