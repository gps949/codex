use codex_login::ApiAccountFallback;
use codex_login::ApiAccountSelection;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::mount_response_sequence;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

use super::add_api_fixture;
use super::api_store;
use super::mount_subscription_usage_denials;
use super::turn_events;
use super::write_account_pool_fixture;

enum Revocation {
    FallbackOff,
    Disabled,
    Removed,
    ModelChanged,
    ManualSelection,
}

async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests(
    revocation: Revocation,
) -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    mount_subscription_usage_denials(&subscription).await;
    let rejected = ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic quota exhaustion",
        "resets_at": chrono::Utc::now().timestamp() + 3_600,
    }}));
    mount_response_sequence(&subscription, vec![rejected.clone(), rejected]).await;
    let endpoint = api.uri();
    let backend_base_url = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            write_account_pool_fixture(home);
            let account = add_api_fixture(home, &endpoint);
            api_store(home)
                .configure_fallback(ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 1,
                })
                .expect("configure synthetic API fallback");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.window_warmup = Some(false);
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(codex_core::TurnInputRequest::user_input(vec![
            UserInput::Text {
                text: "wait for free quota before entering paid API usage".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::Warning(warning) if warning.message.contains("Waiting to retry"))
    }).await;
    let store = api_store(fixture.home.path());
    let mut state = store.load()?;
    let mut account = state.accounts.remove(0);
    match revocation {
        Revocation::FallbackOff => {
            state.fallback.enabled = false;
            store.configure_fallback(state.fallback)?;
        }
        Revocation::Disabled => {
            account.disabled = true;
            store.update(account)?;
        }
        Revocation::Removed => store.remove(&account.id)?,
        Revocation::ModelChanged => {
            account.model = "newly-selected-model".into();
            store.update(account)?;
        }
        Revocation::ManualSelection => store.select(ApiAccountSelection::Manual {
            profile_id: account.id,
        })?,
    }
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        turn_events(&fixture.codex),
    )
    .await??;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_)))
    );
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests_off()
-> anyhow::Result<()> {
    revoking_unstarted_fallback_during_wait_stops_without_paid_requests(Revocation::FallbackOff)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests_disabled()
-> anyhow::Result<()> {
    revoking_unstarted_fallback_during_wait_stops_without_paid_requests(Revocation::Disabled).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests_removed()
-> anyhow::Result<()> {
    revoking_unstarted_fallback_during_wait_stops_without_paid_requests(Revocation::Removed).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests_changed_model()
-> anyhow::Result<()> {
    revoking_unstarted_fallback_during_wait_stops_without_paid_requests(Revocation::ModelChanged)
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_unstarted_fallback_during_wait_stops_without_paid_requests_manual_selection()
-> anyhow::Result<()> {
    revoking_unstarted_fallback_during_wait_stops_without_paid_requests(Revocation::ManualSelection)
        .await
}
