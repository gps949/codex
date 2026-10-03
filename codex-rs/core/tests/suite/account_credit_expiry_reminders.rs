//! Real user-turn coverage for passive account-pool expiry reminders.

use std::sync::Arc;
use std::time::Duration;

use codex_core::TurnInputRequest;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::write_backup_only_account_pool_fixture;

async fn events_for_turn(
    thread: &Arc<codex_core::CodexThread>,
    text: &str,
) -> anyhow::Result<Vec<EventMsg>> {
    thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        loop {
            let event = thread.next_event().await?.msg;
            let complete = matches!(event, EventMsg::TurnComplete(_));
            events.push(event);
            if complete {
                return Ok::<_, anyhow::Error>(events);
            }
        }
    })
    .await?
}

#[tokio::test]
async fn credit_expiry_real_turn_warning_is_durable_model_free_and_does_not_redeem()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let expiry = chrono::Utc::now() + chrono::Duration::hours(4);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "available_count": 1, "credits": [{
                "id": "synthetic-voucher", "reset_type": "codex_rate_limits", "status": "available",
                "granted_at": chrono::Utc::now().to_rfc3339(), "expires_at": expiry.to_rfc3339(),
                "title": "Synthetic weekly reset"
            }],
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    // Keep inference open long enough for the independent metadata pass.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            responses::sse_response(sse(vec![
                ev_response_created("first-response"),
                ev_assistant_message("first-message", "Task complete"),
                ev_completed("first-response"),
            ]))
            .set_delay(Duration::from_millis(400)),
        )
        .expect(/*requests*/ 1)
        .up_to_n_times(/*n*/ 1)
        .mount(&server)
        .await;
    let backend_base_url = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(write_backup_only_account_pool_fixture)
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
            config.model_provider.supports_websockets = false;
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let events = events_for_turn(&fixture.codex, "Complete the real user task").await?;
    let warning = events
        .iter()
        .filter_map(|event| match event {
            EventMsg::Warning(warning) if warning.message.starts_with("Codex reset credit") => {
                Some(&warning.message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(warning.len(), 1);
    assert!(warning[0].contains("backup-acct"));
    assert!(warning[0].contains("No credit was used"));
    assert!(
        fixture
            .codex_home_path()
            .join(".credit-expiry-reminders.json")
            .is_file()
    );
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let body = requests[0].body_json().to_string();
    assert!(body.contains("Complete the real user task"));
    assert!(!body.contains("Codex reset credit"));
    assert!(!body.contains("synthetic-voucher"));
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| {
                request.method.as_str() != "POST" || request.url.path() == "/v1/responses"
            })
    );
    let second = mount_sse_once(&server, sse(vec![ev_completed("second-response")])).await;
    let second_events = events_for_turn(&fixture.codex, "Continue the task").await?;
    assert!(!second_events.iter().any(|event| matches!(event, EventMsg::Warning(warning) if warning.message.starts_with("Codex reset credit"))));
    assert!(
        !second
            .single_request()
            .body_json()
            .to_string()
            .contains("Codex reset credit")
    );
    Ok(())
}
