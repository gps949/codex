use std::path::Path;

use codex_config::types::AuthCredentialsStoreMode;
use codex_login::ApiAccount;
use codex_login::ApiAccountFallback;
use codex_login::ApiAccountSelection;
use codex_login::ApiAccountStore;
use codex_login::AuthKeyringBackendKind;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::write_account_pool_fixture;

#[path = "api_account_compaction.rs"]
mod compaction;
#[path = "api_account_consent.rs"]
mod consent;

fn api_store(home: &Path) -> ApiAccountStore {
    ApiAccountStore::new(
        home.to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
}

fn add_api_fixture(home: &Path, base_url: &str) -> ApiAccount {
    api_store(home)
        .add(
            ApiAccount {
                id: String::new(),
                label: "Synthetic provider".into(),
                base_url: base_url.into(),
                model: "vendor-exact-model".into(),
                disabled: false,
                context_window: 32_768,
                images: false,
            },
            "synthetic-api-key",
        )
        .expect("synthetic API fixture")
}

async fn mount_subscription_usage_denials(server: &MockServer) {
    for (profile_id, access_token) in [
        ("primary-acct", "access-primary"),
        ("backup-acct", "access-backup"),
    ] {
        let account_id = format!("account-{profile_id}");
        let user_id = format!("user-{profile_id}");
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .and(header("authorization", format!("Bearer {access_token}")))
            .and(header("chatgpt-account-id", account_id.clone()))
            .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
                "plan_type": "pro", "account_id": account_id, "user_id": user_id,
                "rate_limit": {"allowed": false, "limit_reached": true,
                    "primary_window": {"used_percent": 100, "limit_window_seconds": 18000,
                        "reset_after_seconds": 3600, "reset_at": chrono::Utc::now().timestamp() + 3600}},
            })))
            .mount(server)
            .await;
    }
}

async fn assert_all_subscription_seats_checked(server: &MockServer) {
    let mut checked_seats = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "GET" && request.url.path() == "/backend-api/wham/usage"
        })
        .map(|request| {
            (
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                request
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    checked_seats.sort_unstable();
    checked_seats.dedup();
    assert_eq!(
        checked_seats,
        vec![
            (
                Some("Bearer access-backup".to_string()),
                Some("account-backup-acct".to_string()),
            ),
            (
                Some("Bearer access-primary".to_string()),
                Some("account-primary-acct".to_string()),
            ),
        ]
    );
}

async fn turn_events(thread: &codex_core::CodexThread) -> anyhow::Result<Vec<EventMsg>> {
    let mut events = Vec::new();
    loop {
        let event = thread.next_event().await?.msg;
        let complete = matches!(event, EventMsg::TurnComplete(_));
        events.push(event);
        if complete {
            return Ok(events);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_api_selection_binds_key_and_model_across_tool_steps() -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    let requests = mount_sse_sequence(
        &api,
        vec![
            sse(vec![
                ev_response_created("api-tool-response"),
                json!({"type": "response.output_item.done", "item": {
                    "type": "reasoning", "id": "api-reasoning",
                    "summary": [{"type": "summary_text", "text": "Complete the plan"}],
                    "encrypted_content": "opaque-api-reasoning",
                }}),
                ev_function_call(
                    "api-plan-call",
                    "update_plan",
                    &json!({"plan": [{"step": "Keep the API target", "status": "completed"}]})
                        .to_string(),
                ),
                ev_completed("api-tool-response"),
            ]),
            sse(vec![
                ev_response_created("api-final-response"),
                ev_assistant_message("api-message", "completed on the selected provider"),
                ev_completed("api-final-response"),
            ]),
        ],
    )
    .await;
    let base_url = api.uri();
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &base_url);
            api_store(home)
                .select(ApiAccountSelection::Manual {
                    profile_id: account.id,
                })
                .expect("select synthetic API fixture");
        })
        .with_config(|config| {
            config.account_pool.window_warmup = Some(false);
            config.update_plan_enabled = true;
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .submit_turn("finish the plan using the selected API account")
        .await?;
    let requests = requests.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.body_json()["model"], json!("vendor-exact-model"));
        assert_eq!(
            request.header("authorization").as_deref(),
            Some("Bearer synthetic-api-key")
        );
        assert_eq!(request.header("chatgpt-account-id"), None);
        assert_eq!(request.header("x-openai-account-routing-override"), None);
        assert_eq!(request.header("x-openai-actor-authorization"), None);
        assert_eq!(request.body_json()["include"], json!([]));
        assert!(request.body_json()["reasoning"].is_null());
        assert!(request.body_json()["client_metadata"].is_null());
    }
    assert!(!requests[1].function_call_output("api-plan-call").is_null());
    assert!(
        !requests[1]
            .body_json()
            .to_string()
            .contains("opaque-api-reasoning")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opt_in_api_fallback_preserves_completed_tools_and_subscription_selection()
-> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic quota exhaustion",
        "resets_at": chrono::Utc::now().timestamp() + 3_600,
    }}));
    let subscription_requests = mount_response_sequence(
        &subscription,
        vec![
            responses::sse_response(sse(vec![
                ev_response_created("subscription-tool-response"),
                ev_function_call(
                    "subscription-plan-call",
                    "update_plan",
                    &json!({"plan": [{"step": "Run this tool once", "status": "completed"}]})
                        .to_string(),
                ),
                ev_completed("subscription-tool-response"),
            ])),
            rejected.clone(),
            rejected,
        ],
    )
    .await;
    let api_requests = mount_sse_once(
        &api,
        sse(vec![
            ev_response_created("fallback-response"),
            ev_assistant_message("fallback-message", "continued from the completed tool"),
            ev_completed("fallback-response"),
        ]),
    )
    .await;
    let base_url = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &base_url);
            api_store(home)
                .configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 0,
                })
                .expect("configure synthetic fallback");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
            config.update_plan_enabled = true;
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            codex_protocol::user_input::UserInput::Text {
                text: "run the plan once and complete the task".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    let events = turn_events(&fixture.codex).await?;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_))),
        "errors={:?}",
        events
            .iter()
            .filter(|event| matches!(event, EventMsg::Error(_)))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EventMsg::PlanUpdate(_)))
            .count(),
        1
    );
    assert_eq!(subscription_requests.requests().len(), 3);
    assert_all_subscription_seats_checked(&subscription).await;
    let request = api_requests.single_request();
    assert_eq!(request.body_json()["model"], json!("vendor-exact-model"));
    assert!(
        !request
            .function_call_output("subscription-plan-call")
            .is_null()
    );
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer synthetic-api-key")
    );
    assert_eq!(request.header("chatgpt-account-id"), None);
    assert_eq!(
        api_store(fixture.home.path()).load()?.selection,
        ApiAccountSelection::Subscription
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_subscription_pool_does_not_use_api_by_default() -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic quota exhaustion",
        "resets_at": chrono::Utc::now().timestamp() + 3_600,
    }}));
    let requests = mount_response_sequence(&subscription, vec![rejected.clone(), rejected]).await;
    let base_url = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            add_api_fixture(home, &base_url);
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            codex_protocol::user_input::UserInput::Text {
                text: "stop when the subscription pool is exhausted".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    let events = turn_events(&fixture.codex).await?;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_)))
    );
    assert_eq!(requests.requests().len(), 2);
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn api_fallback_wait_can_be_cancelled_without_paid_requests() -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic quota exhaustion",
        "resets_at": chrono::Utc::now().timestamp() + 3_600,
    }}));
    mount_response_sequence(&subscription, vec![rejected.clone(), rejected]).await;
    let base_url = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &base_url);
            api_store(home)
                .configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 1,
                })
                .expect("configure synthetic fallback");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            codex_protocol::user_input::UserInput::Text {
                text: "wait before entering the paid fallback".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::Warning(warning) if warning.message.contains("Waiting to retry"))
    }).await;
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    fixture
        .codex
        .submit(codex_protocol::protocol::Op::Interrupt)
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_api_fallback_does_not_block_subscription_usage() -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    let request = mount_sse_once(
        &subscription,
        sse(vec![
            ev_response_created("subscription-only"),
            ev_assistant_message("subscription-message", "completed with subscription"),
            ev_completed("subscription-only"),
        ]),
    )
    .await;
    let base_url = api.uri();
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let mut account = add_api_fixture(home, &base_url);
            api_store(home)
                .configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id.clone()),
                    wait_minutes: 0,
                })
                .unwrap();
            account.disabled = true;
            api_store(home).update(account).unwrap();
        })
        .with_config(|config| config.account_pool.window_warmup = Some(false));
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture.submit_turn("use my available subscription").await?;
    assert_eq!(
        request.single_request().header("authorization").as_deref(),
        Some("Bearer access-primary")
    );
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabling_all_subscription_accounts_during_wait_does_not_enter_paid_fallback()
-> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(429)
        .set_body_json(json!({"error": {"type":"usage_limit_reached",
        "message":"synthetic quota exhaustion","resets_at":chrono::Utc::now().timestamp()+3600}}));
    mount_response_sequence(&subscription, vec![rejected.clone(), rejected]).await;
    let base_url = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &base_url);
            api_store(home)
                .configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 1,
                })
                .unwrap();
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            codex_protocol::user_input::UserInput::Text {
                text: "wait for subscription quota".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    wait_for_event(&fixture.codex, |event| matches!(event, EventMsg::Warning(warning) if warning.message.contains("Waiting to retry"))).await;
    let profiles = codex_login::AccountProfileStore::new(fixture.home.path().to_path_buf());
    for record in profiles.load_profile_records()? {
        profiles.update_profile_metadata(
            &record.profile.id,
            codex_login::AccountProfileMetadataUpdate {
                disabled: Some(true),
                ..Default::default()
            },
        )?;
    }
    let events = turn_events(&fixture.codex).await?;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_)))
    );
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    Ok(())
}
