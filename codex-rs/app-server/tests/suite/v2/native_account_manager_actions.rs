use super::*;
use codex_app_server_protocol::TurnSteerResponse;
use pretty_assertions::assert_eq;
use wiremock::ResponseTemplate;
use wiremock::matchers::path;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_disable_requires_confirmation_and_reloads_the_selected_account() -> Result<()>
{
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let manifest = fixture.home.path().join("account-profiles.json");
    let before = std::fs::read(&manifest)?;
    let (turn, id, question) = fixture.account_actions().await?;
    let (id, question) = fixture.choose(id, &question, "More actions").await?;
    let (id, question) = fixture.choose(id, &question, "Enable / remove").await?;
    let (id, question) = fixture.choose(id, &question, "Disable").await?;
    assert_eq!(std::fs::read(&manifest)?, before);
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("Work fixture")
    );
    let (id, question) = fixture.choose(id, &question, "Confirm").await?;
    let stored: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    assert_eq!(stored["profiles"][0]["disabled"], json!(true));
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("saved")
    );
    let (id, question) = fixture.choose(id, &question, "Continue").await?;
    let (id, question) = fixture.choose(id, &question, "Account actions").await?;
    let (id, question) = fixture.choose(id, &question, "More actions").await?;
    let (id, question) = fixture.choose(id, &question, "Enable / remove").await?;
    assert!(
        question.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| option.label == "Enable")
    );
    let (id, question) = fixture.choose(id, &question, "Back").await?;
    let (id, question) = fixture.choose(id, &question, "Back").await?;
    fixture
        .answer(id.clone(), &question.questions[0].id, vec!["Close".into()])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_rename_uses_separate_text_then_confirm_and_keeps_selection() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let manifest = fixture.home.path().join("account-profiles.json");
    let (turn, id, question) = fixture.account_actions().await?;
    let (id, question) = fixture.choose(id, &question, "More actions").await?;
    let (id, question) = fixture.choose(id, &question, "Rename").await?;
    assert!(question.questions[0].options.is_none());
    fixture
        .answer(
            id,
            &question.questions[0].id,
            vec!["Main Business seat".into()],
        )
        .await?;
    let (id, question) = fixture.read_question().await?;
    let before: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    assert_eq!(before["profiles"][0]["label"], "Work fixture");
    let (id, question) = fixture.choose(id, &question, "Confirm").await?;
    let stored: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    assert_eq!(stored["profiles"][0]["label"], "Main Business seat");
    let (id, question) = fixture.choose(id, &question, "Continue").await?;
    assert_eq!(question.questions[0].question, "Main Business seat");
    fixture
        .answer(id.clone(), &question.questions[0].id, vec!["Close".into()])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    let runtime: Value = serde_json::from_slice(&std::fs::read(
        fixture.home.path().join("account-runtime-state.json"),
    )?)?;
    assert_eq!(runtime["active_profile_id"], PROFILE_ID);
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_rejects_same_email_new_workspace_after_confirmation_is_displayed() -> Result<()>
{
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let manifest = fixture.home.path().join("account-profiles.json");
    let (turn, id, question) = fixture.account_actions().await?;
    let (id, question) = fixture.choose(id, &question, "More actions").await?;
    let (id, question) = fixture.choose(id, &question, "Enable / remove").await?;
    let (id, question) = fixture
        .choose(id, &question, "Remove, keep credentials")
        .await?;
    let before = std::fs::read(&manifest)?;
    write_chatgpt_auth(
        &fixture.home.path().join("auth-profiles").join(PROFILE_ID),
        ChatGptAuthFixture::new("native-new-owner-access")
            .account_id("different-workspace")
            .chatgpt_account_id("different-workspace")
            .chatgpt_user_id("native-fixture-user")
            .email("native-fixture@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let (id, question) = fixture.choose(id, &question, "Confirm").await?;
    assert_eq!(std::fs::read(&manifest)?, before);
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("workspace changed")
    );
    fixture
        .answer(id.clone(), &question.questions[0].id, vec!["Close".into()])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_attaches_to_running_turn_without_model_steering_or_synthetic_completion()
-> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            core_test_support::responses::sse_response(
                create_final_assistant_message_sse_response("real task completed")?,
            )
            .set_delay(Duration::from_secs(3)),
        )
        .expect(1)
        .mount(&fixture.backend)
        .await;
    let real = fixture.start("work while I inspect accounts").await?;
    let _: TurnStartedNotification =
        timeout(READ_TIMEOUT, fixture.app.read_notification("turn/started")).await??;
    let id = fixture
        .app
        .send_raw_request(
            "turn/steer",
            Some(json!({
                "threadId": fixture.thread_id, "expectedTurnId": real.turn.id,
                "input": [{"type": "text", "text": "/account manage", "textElements": []}]
            })),
        )
        .await?;
    let steered: TurnSteerResponse = timeout(READ_TIMEOUT, fixture.app.read_response(id)).await??;
    assert_eq!(steered.turn_id, real.turn.id);
    let (id, question) = fixture.read_question().await?;
    assert_eq!(question.turn_id, real.turn.id);
    assert!(!question.is_blocking);
    let (id, question) = fixture.choose(id, &question, "Accounts").await?;
    assert_eq!(question.turn_id, real.turn.id);
    let (id, question) = fixture.choose(id, &question, "1. Work fixture").await?;
    fixture
        .answer(id, &question.questions[0].id, vec!["Close".into()])
        .await?;
    let completed: TurnCompletedNotification = timeout(
        READ_TIMEOUT,
        fixture.app.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(
        (completed.turn.id, completed.turn.status),
        (real.turn.id, TurnStatus::Completed)
    );
    let requests = fixture.inference_requests().await;
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    let input = serde_json::to_string(&body["input"])?;
    assert!(input.contains("work while I inspect accounts"));
    assert!(!input.contains("/account"));
    assert!(!input.contains("Account controls"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_credit_read_is_nonconsuming_and_redemption_cancel_sends_no_post() -> Result<()>
{
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    Mock::given(method("GET")).and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "credits": [{"id":"native-credit","reset_type":"codex_rate_limits","status":"available","expires_at":"2099-01-01T00:00:00Z","granted_at":"2026-10-03T00:00:00Z"}],
            "available_count":1,"total_earned_count":1
        }))).mount(&fixture.backend).await;
    let (turn, id, question) = fixture.account_actions().await?;
    let (id, question) = fixture.choose(id, &question, "Reset credits").await?;
    let (id, question) = fixture.choose(id, &question, "Credit 1").await?;
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("Work fixture")
    );
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("2099-01-01")
    );
    let (id, question) = fixture.choose(id, &question, "Cancel").await?;
    let (id, question) = fixture.choose(id, &question, "Back").await?;
    fixture
        .answer(id.clone(), &question.questions[0].id, vec!["Close".into()])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    assert!(
        fixture
            .backend
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .all(|request| request.method != "POST")
    );
    Ok(())
}

struct DelayedCreditCheck {
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl wiremock::Respond for DelayedCreditCheck {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let response = ResponseTemplate::new(200).set_body_json(json!({
            "credits": [{"id":"native-credit","reset_type":"codex_rate_limits","status":"available","expires_at":"2099-01-01T00:00:00Z","granted_at":"2026-10-03T00:00:00Z"}],
            "available_count":1,"total_earned_count":1
        }));
        if call == 0 {
            response
        } else {
            response.set_delay(Duration::from_secs(3))
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_stop_cancels_confirmed_redemption_before_delayed_credit_check_finishes()
-> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(DelayedCreditCheck {
            calls: std::sync::Arc::clone(&calls),
        })
        .expect(2)
        .mount(&fixture.backend)
        .await;
    let (turn, id, question) = fixture.account_actions().await?;
    let (id, question) = fixture.choose(id, &question, "Reset credits").await?;
    let (id, question) = fixture.choose(id, &question, "Credit 1").await?;
    fixture
        .answer(
            id.clone(),
            &question.questions[0].id,
            vec!["Confirm".into()],
        )
        .await?;
    timeout(READ_TIMEOUT, async {
        while calls.load(std::sync::atomic::Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let interrupt = fixture
        .app
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: fixture.thread_id.clone(),
            turn_id: turn.turn.id.clone(),
        })
        .await?;
    let _: TurnInterruptResponse =
        timeout(Duration::from_secs(1), fixture.app.read_response(interrupt)).await??;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    // Observe beyond the delayed response: a stopped menu cannot later launch consumption.
    tokio::time::sleep(Duration::from_millis(3200)).await;
    assert!(
        fixture
            .backend
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .all(|request| request.method != "POST")
    );
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_menu_real_turn_completion_cancels_confirmed_redemption_before_submission()
-> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            core_test_support::responses::sse_response(
                create_final_assistant_message_sse_response("real task completed")?,
            )
            .set_delay(Duration::from_secs(1)),
        )
        .expect(1)
        .mount(&fixture.backend)
        .await;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(DelayedCreditCheck {
            calls: std::sync::Arc::clone(&calls),
        })
        .expect(2)
        .mount(&fixture.backend)
        .await;
    let turn = fixture
        .start("finish while I inspect a reset credit")
        .await?;
    let _: TurnStartedNotification =
        timeout(READ_TIMEOUT, fixture.app.read_notification("turn/started")).await??;
    let steer = fixture
        .app
        .send_raw_request(
            "turn/steer",
            Some(json!({
                "threadId":fixture.thread_id,"expectedTurnId":turn.turn.id,
                "input":[{"type":"text","text":"/account manage","textElements":[]}]
            })),
        )
        .await?;
    let _: TurnSteerResponse = timeout(READ_TIMEOUT, fixture.app.read_response(steer)).await??;
    let (id, question) = fixture.read_question().await?;
    let (id, question) = fixture.choose(id, &question, "Accounts").await?;
    let (id, question) = fixture.choose(id, &question, "1. Work fixture").await?;
    let (id, question) = fixture.choose(id, &question, "Account actions").await?;
    let (id, question) = fixture.choose(id, &question, "Reset credits").await?;
    let (id, question) = fixture.choose(id, &question, "Credit 1").await?;
    fixture
        .answer(id, &question.questions[0].id, vec!["Confirm".into()])
        .await?;
    timeout(READ_TIMEOUT, async {
        while calls.load(std::sync::atomic::Ordering::Relaxed) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let completed: TurnCompletedNotification = timeout(
        READ_TIMEOUT,
        fixture.app.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(
        (completed.turn.id, completed.turn.status),
        (turn.turn.id, TurnStatus::Completed)
    );
    tokio::time::sleep(Duration::from_millis(3200)).await;
    assert!(
        fixture
            .backend
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| !request.url.path().ends_with("/responses"))
            .all(|request| request.method != "POST")
    );
    assert_eq!(fixture.inference_requests().await.len(), 1);
    Ok(())
}
