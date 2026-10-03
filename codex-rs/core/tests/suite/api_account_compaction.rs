use codex_login::ApiAccountFallback;
use codex_login::ApiAccountSelection;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use super::super::account_failover::latest_compaction_summary_execution_provenance;
use super::add_api_fixture;
use super::api_store;
use super::assert_all_subscription_seats_checked;
use super::mount_subscription_usage_denials;
use super::turn_events;
use super::write_account_pool_fixture;

fn assert_api_request(request: &ResponsesRequest) {
    assert_eq!(request.body_json()["model"], json!("vendor-exact-model"));
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer synthetic-api-key")
    );
    assert_eq!(request.header("chatgpt-account-id"), None);
    assert_eq!(request.header("x-openai-account-routing-override"), None);
    assert_eq!(request.header("x-openai-actor-authorization"), None);
    assert!(request.body_json()["reasoning"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_captures_api_selection_and_projects_subscription_history()
-> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    let subscription_requests = mount_sse_sequence(
        &subscription,
        vec![sse(vec![
            ev_response_created("subscription-seed"),
            json!({"type": "response.output_item.done", "item": {
                "type": "reasoning", "id": "foreign-reasoning",
                "summary": [{"type": "summary_text", "text": "Keep readable seed context"}],
                "encrypted_content": "foreign-subscription-secret",
            }}),
            ev_assistant_message("subscription-message", "seed context"),
            ev_completed("subscription-seed"),
        ])],
    )
    .await;
    let api_requests = mount_sse_sequence(
        &api,
        vec![
            sse(vec![
                ev_response_created("manual-api-compact"),
                ev_assistant_message("manual-api-summary", "portable API summary"),
                ev_completed("manual-api-compact"),
            ]),
            sse(vec![
                ev_response_created("after-manual-compact"),
                ev_assistant_message("after-manual-message", "continued with the summary"),
                ev_completed("after-manual-compact"),
            ]),
        ],
    )
    .await;
    let endpoint = api.uri();
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            add_api_fixture(home, &endpoint);
        })
        .with_config(|config| config.account_pool.window_warmup = Some(false));
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture.submit_turn("retain this user context").await?;
    let store = api_store(fixture.home.path());
    let account = store.load()?.accounts.remove(0);
    store.select(ApiAccountSelection::Manual {
        profile_id: account.id.clone(),
    })?;
    fixture.codex.submit(Op::Compact).await?;
    let events = turn_events(&fixture.codex).await?;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_)))
    );
    fixture.submit_turn("continue the original task").await?;
    assert_eq!(subscription_requests.requests().len(), 1);
    let requests = api_requests.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_api_request(request);
        assert!(
            !request
                .body_json()
                .to_string()
                .contains("foreign-subscription-secret")
        );
    }
    assert!(
        requests[0]
            .body_json()
            .to_string()
            .contains("Keep readable seed context")
    );
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("portable API summary")
    );
    assert!(
        requests[1]
            .body_json()
            .to_string()
            .contains("retain this user context")
    );
    let rollout = fixture
        .session_configured
        .rollout_path
        .as_deref()
        .expect("configure synthetic API selection");
    assert!(std::fs::read_to_string(rollout)?.contains("foreign-subscription-secret"));
    assert_eq!(
        latest_compaction_summary_execution_provenance(rollout)?,
        (Some(account.id), Some(0)),
    );
    Ok(())
}

enum ApiMode {
    Manual,
    Fallback,
}

async fn automatic_compaction_keeps_captured_api_transport_and_completed_tools(
    mode: ApiMode,
) -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic exhausted subscription",
        "resets_at": chrono::Utc::now().timestamp() + 3_600,
    }}));
    let subscription_requests = if matches!(mode, ApiMode::Fallback) {
        Some(mount_response_sequence(&subscription, vec![rejected.clone(), rejected]).await)
    } else {
        None
    };
    let requests = mount_sse_sequence(
        &api,
        vec![
            sse(vec![
                ev_response_created("large-api-tool-response"),
                ev_function_call(
                    "api-tool-before-compact",
                    "update_plan",
                    &json!({"plan": [{"step": "Preserve completed work", "status": "completed"}]})
                        .to_string(),
                ),
                ev_completed_with_tokens("large-api-tool-response", /*total_tokens*/ 40_000),
            ]),
            sse(vec![
                ev_response_created("automatic-api-compact"),
                ev_assistant_message("automatic-api-summary", "The plan was updated exactly once"),
                ev_completed_with_tokens("automatic-api-compact", /*total_tokens*/ 20),
            ]),
            sse(vec![
                ev_response_created("automatic-api-followup"),
                ev_assistant_message("automatic-api-final", "completed after API compaction"),
                ev_completed("automatic-api-followup"),
            ]),
        ],
    )
    .await;
    let expected_subscription_requests = match mode {
        ApiMode::Manual => 0,
        ApiMode::Fallback => 2,
    };
    let endpoint = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &endpoint);
            let store = api_store(home);
            match mode {
                ApiMode::Manual => store.select(ApiAccountSelection::Manual {
                    profile_id: account.id,
                }),
                ApiMode::Fallback => store.configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 0,
                }),
            }
            .expect("configure synthetic API selection");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
            config.update_plan_enabled = true;
            config.model_post_turn_compact_threshold_percent = 0;
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .submit_turn("keep the plan through a full context window")
        .await?;
    assert_eq!(
        subscription_requests
            .as_ref()
            .map_or(0, |mock| mock.requests().len()),
        expected_subscription_requests
    );
    if expected_subscription_requests > 0 {
        assert_all_subscription_seats_checked(&subscription).await;
    }
    let requests = requests.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_api_request(request);
    }
    assert!(
        requests[1]
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains(codex_prompts::SUMMARIZATION_PROMPT))
    );
    assert!(
        !requests[1]
            .function_call_output("api-tool-before-compact")
            .is_null()
    );
    assert!(
        requests[2]
            .body_json()
            .to_string()
            .contains("The plan was updated exactly once")
    );
    let account = api_store(fixture.home.path()).load()?.accounts.remove(0);
    assert_eq!(
        latest_compaction_summary_execution_provenance(
            fixture
                .session_configured
                .rollout_path
                .as_deref()
                .expect("synthetic rollout path"),
        )?,
        (Some(account.id), Some(0)),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_compaction_keeps_captured_api_transport_and_completed_tools_manual()
-> anyhow::Result<()> {
    automatic_compaction_keeps_captured_api_transport_and_completed_tools(ApiMode::Manual).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_compaction_keeps_captured_api_transport_and_completed_tools_fallback()
-> anyhow::Result<()> {
    automatic_compaction_keeps_captured_api_transport_and_completed_tools(ApiMode::Fallback).await
}
