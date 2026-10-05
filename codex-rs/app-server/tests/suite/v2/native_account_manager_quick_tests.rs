use super::*;
use codex_app_server_protocol::TurnSteerResponse;
use pretty_assertions::assert_eq;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::path;

fn enroll_extra_accounts(fixture: &NativeFixture) -> Result<()> {
    let manifest = fixture.home.path().join("account-profiles.json");
    let mut data: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
    for index in 1..=5 {
        let profile_id = format!("extra-{index}");
        let home = fixture.home.path().join("auth-profiles").join(&profile_id);
        std::fs::create_dir_all(&home)?;
        write_chatgpt_auth(
            &home,
            ChatGptAuthFixture::new(format!("native-extra-{index}-access"))
                .account_id(format!("native-extra-workspace-{index}"))
                .chatgpt_account_id(format!("native-extra-workspace-{index}"))
                .chatgpt_user_id(format!("native-extra-user-{index}"))
                .email(format!("native-extra-{index}@example.com"))
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
        data["profiles"]
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("Fixture manifest requires a profile list"))?
            .push(json!({
                "id": profile_id, "label": format!("Profile {index}"), "priority": index,
                "credential_location": "managed_profile", "state": "ready", "disabled": false
            }));
    }
    std::fs::write(manifest, serde_json::to_vec(&data)?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_quick_refreshes_only_the_visible_page_and_returns_there() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    enroll_extra_accounts(&fixture)?;
    for index in [4, 5] {
        Mock::given(method("GET")).and(path("/api/codex/usage"))
            .and(header("chatgpt-account-id", format!("native-extra-workspace-{index}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "account_id": format!("native-extra-workspace-{index}"), "user_id": format!("native-extra-user-{index}"), "plan_type": "pro",
                "rate_limit": {"allowed": true, "limit_reached": false,
                    "primary_window": {"used_percent": 8, "reset_at": chrono::Utc::now().timestamp() + 7200, "limit_window_seconds": 18000, "reset_after_seconds": 7200},
                    "secondary_window": {"used_percent": 12, "reset_at": chrono::Utc::now().timestamp() + 7200, "limit_window_seconds": 604800, "reset_after_seconds": 7200}}
            }))).expect(1).mount(&fixture.backend).await;
    }
    let turn = fixture.start("/account").await?;
    let (id, question) = fixture.read_question().await?;
    let (id, question) = fixture.choose(id, &question, "More").await?;
    let (id, question) = fixture.choose(id, &question, "Next page").await?;
    let (id, question) = fixture.choose(id, &question, "More").await?;
    let (id, question) = fixture.choose(id, &question, "Next page").await?;
    let (id, question) = fixture.choose(id, &question, "More").await?;
    let (id, question) = fixture.choose(id, &question, "Refresh this page").await?;
    assert_eq!(question.questions[0].question, "Accounts · 3/3");
    let options = question.questions[0].options.as_ref().unwrap();
    assert_eq!(
        (&options[0].label, &options[1].label),
        (&"5. Profile 4".to_string(), &"6. Profile 5".to_string())
    );
    assert!(options[0].description.contains("8% used"), "{question:?}");
    fixture
        .answer(id.clone(), &question.questions[0].id, vec![])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    let requests = fixture
        .backend
        .received_requests()
        .await
        .unwrap_or_default();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/api/codex/usage")
            .count(),
        2
    );
    assert!(requests.iter().all(|request| request.method != "POST"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_management_reload_preserves_the_current_list_page() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    enroll_extra_accounts(&fixture)?;
    let (turn, id, question) = fixture.open_menu().await?;
    let (id, question) = fixture.choose(id, &question, "Accounts").await?;
    let (id, question) = fixture.choose(id, &question, "More / pages").await?;
    let (id, question) = fixture.choose(id, &question, "Next page").await?;
    let (id, question) = fixture.choose(id, &question, "More / pages").await?;
    let (id, question) = fixture.choose(id, &question, "Reload inventory").await?;
    assert_eq!(question.questions[0].question, "Account list options");
    let (id, question) = fixture.choose(id, &question, "Choose page").await?;
    let (id, question) = fixture.choose(id, &question, "Back").await?;
    assert_eq!(question.questions[0].question, "Choose an account · 2/2");
    assert_eq!(
        question.questions[0].options.as_ref().unwrap()[0].label,
        "5. Profile 4"
    );
    fixture
        .answer(id.clone(), &question.questions[0].id, vec![])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_quick_selection_preserves_the_attached_real_turn() -> Result<()> {
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
    let turn = fixture.start("continue while I select an account").await?;
    let _: TurnStartedNotification =
        timeout(READ_TIMEOUT, fixture.app.read_notification("turn/started")).await??;
    let steer = fixture
        .app
        .send_raw_request(
            "turn/steer",
            Some(json!({
                "threadId": fixture.thread_id, "expectedTurnId": turn.turn.id,
                "input": [{"type": "text", "text": "/account", "textElements": []}]
            })),
        )
        .await?;
    let result: TurnSteerResponse =
        timeout(READ_TIMEOUT, fixture.app.read_response(steer)).await??;
    assert_eq!(result.turn_id, turn.turn.id);
    let (id, question) = fixture.read_question().await?;
    assert!(!question.is_blocking);
    assert_eq!(question.turn_id, turn.turn.id);
    fixture
        .answer(
            id,
            &question.questions[0].id,
            vec!["1. Work fixture".into()],
        )
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
    let requests = fixture.inference_requests().await;
    assert_eq!(requests.len(), 1);
    let input =
        serde_json::to_string(&serde_json::from_slice::<Value>(&requests[0].body)?["input"])?;
    assert!(!input.contains("/account"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_quick_selects_the_displayed_subscription_without_confirmation() -> Result<()>
{
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let turn = fixture.start("/account").await?;
    let (id, question) = fixture.read_question().await?;
    let options = question.questions[0].options.as_ref().unwrap();
    assert!(
        options
            .iter()
            .any(|option| option.label == "1. Work fixture"
                && option.description.contains("native-fixture@example.com"))
    );
    assert!(
        options
            .iter()
            .any(|option| option.label == "Choose automatically")
    );
    fixture
        .answer(
            id.clone(),
            &question.questions[0].id,
            vec!["1. Work fixture".into()],
        )
        .await?;
    let completed = fixture.finish_menu(&turn.turn.id, &id).await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let runtime: Value = serde_json::from_slice(&std::fs::read(
        fixture.home.path().join("account-runtime-state.json"),
    )?)?;
    assert_eq!(runtime["active_profile_id"], PROFILE_ID);
    assert!(
        runtime["selection_revision"]
            .as_u64()
            .is_some_and(|revision| revision > 0)
    );
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_quick_strategy_sets_an_explicit_value_without_confirmation() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let turn = fixture.start("/account").await?;
    let (id, question) = fixture.read_question().await?;
    let (id, question) = fixture.choose(id, &question, "Strategy").await?;
    fixture
        .answer(
            id.clone(),
            &question.questions[0].id,
            vec!["By reset time".into()],
        )
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    let config: toml::Value = toml::from_str(&std::fs::read_to_string(
        fixture.home.path().join("config.toml"),
    )?)?;
    assert_eq!(
        config["account_pool"]["rotation_strategy"].as_str(),
        Some("earliest_reset")
    );
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_quick_rechecks_workspace_before_immediate_selection() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let turn = fixture.start("/account").await?;
    let (id, question) = fixture.read_question().await?;
    let runtime = fixture.home.path().join("account-runtime-state.json");
    let before = std::fs::read(&runtime)?;
    write_chatgpt_auth(
        &fixture.home.path().join("auth-profiles").join(PROFILE_ID),
        ChatGptAuthFixture::new("native-replacement-access")
            .account_id("replacement-workspace")
            .chatgpt_account_id("replacement-workspace")
            .chatgpt_user_id("native-fixture-user")
            .email("native-fixture@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let (id, question) = fixture.choose(id, &question, "1. Work fixture").await?;
    assert_eq!(std::fs::read(&runtime)?, before);
    assert!(
        question.questions[0].options.as_ref().unwrap()[0]
            .description
            .contains("workspace changed")
    );
    let (id, question) = fixture.choose(id, &question, "Continue").await?;
    assert!(
        question.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| option.label == "Strategy")
    );
    fixture
        .answer(id.clone(), &question.questions[0].id, vec![])
        .await?;
    fixture.finish_menu(&turn.turn.id, &id).await?;
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}
