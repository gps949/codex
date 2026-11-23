use codex_core::ExecutionAccountPoolHandle;
use codex_core::TurnInputRequest;
use codex_login::AccountAvailability;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_strategy_refresh_changes_next_automatic_execution() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response"),
            responses::ev_assistant_message("message", "done"),
            responses::ev_completed("response"),
        ]),
    )
    .await;
    let mut builder = test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(|home| {
            super::account_failover::write_account_pool_fixture(home);
            let now = chrono::Utc::now();
            let states: Vec<_> = [("primary-acct", 7200), ("backup-acct", 3600)].into_iter().map(|(id, seconds)| json!({
                "profile_id": id,
                "rate_limits": {"observed_at": now, "primary": {"used_percent": 20.0, "resets_at": now + chrono::Duration::seconds(seconds)}}
            })).collect();
            std::fs::write(home.join("account-runtime-state.json"), serde_json::to_vec(&json!({"version": 1, "active_profile_id": "primary-acct", "profiles": states})).unwrap()).unwrap();
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let pool = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    assert!(pool.ensure_from_config(&fixture.config).await?);
    let mut next = fixture.config.clone();
    next.account_pool.rotation_strategy =
        Some(codex_config::AccountPoolRotationStrategy::EarliestReset);
    let current = fixture.codex.config().await;
    assert_eq!(
        fixture.codex.refresh_runtime_config(current, next).await,
        codex_core::ConfigRefreshOutcome::Published
    );
    let effective = fixture.codex.config().await;
    assert_eq!(
        effective.account_pool.effective_rotation_strategy(),
        codex_config::AccountPoolRotationStrategy::EarliestReset
    );
    pool.ensure_from_config(&effective).await?;
    assert_eq!(
        pool.activate_fill_first().await?.profile_id.as_str(),
        "backup-acct"
    );
    fixture
        .submit_turn("use the updated account strategy")
        .await?;
    assert_eq!(
        response.single_request().header("authorization").as_deref(),
        Some("Bearer access-backup")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preemptive_parking_retains_quota_until_all_crossed_windows_reset() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let response = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response"),
            responses::ev_assistant_message("message", "done"),
            responses::ev_completed("response"),
        ]),
    )
    .await;
    let now = chrono::Utc::now();
    let weekly_reset = now + chrono::Duration::days(5);
    let expected_limits = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: Some(now + chrono::Duration::hours(1)),
            window_minutes: Some(300),
        }),
        secondary: Some(AccountRateLimitWindow {
            used_percent: 96.0,
            resets_at: Some(weekly_reset),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),

        window_observed_at: None,
    };
    let saved_limits = expected_limits.clone();
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(move |home| {
            super::account_failover::write_account_pool_fixture(home);
            std::fs::write(
                home.join("account-runtime-state.json"),
                serde_json::to_vec(&json!({
                    "version": 1,
                    "active_profile_id": "primary-acct",
                    "profiles": [{
                        "profile_id": "primary-acct",
                        "rate_limits": saved_limits,
                    }],
                }))
                .expect("serialize fixture quota"),
            )
            .expect("save fixture quota");
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let pool = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    assert!(pool.ensure_from_config(&fixture.config).await?);
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "preserve both quota windows".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let warning = wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::Warning(warning) if warning.message.contains("Switched to Codex account"))
    })
    .await;
    let EventMsg::Warning(warning) = warning else {
        unreachable!("matched warning event");
    };
    assert!(warning.message.contains("99%"));
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let primary = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id.as_str() == "primary-acct")
        .expect("primary profile remains enrolled");
    assert_eq!(
        (
            primary.availability,
            primary.preemptive_rotation_until,
            primary.rate_limits,
            response.single_request().header("authorization"),
        ),
        (
            AccountAvailability::Available,
            Some(weekly_reset),
            expected_limits,
            Some("Bearer access-backup".into()),
        )
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_turn_tool_step_boundary_preemptively_switches_accounts() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let plan_arguments = json!({
        "explanation": "Step 1 crosses the 95% threshold",
        "plan": [{"step": "Run tool", "status": "completed"}],
    });
    let reset_ts = (chrono::Utc::now() + chrono::Duration::hours(2))
        .timestamp()
        .to_string();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-primary",
        ))
        .respond_with(
            responses::sse_response(responses::sse(vec![
                responses::ev_response_created("step-1"),
                responses::ev_function_call("call-1", "update_plan", &plan_arguments.to_string()),
                responses::ev_completed("step-1"),
            ]))
            .insert_header("x-codex-primary-used-percent", "97.0")
            .insert_header("x-codex-primary-window-minutes", "300")
            .insert_header("x-codex-primary-reset-at", reset_ts.as_str()),
        )
        .expect(1)
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-backup",
        ))
        .respond_with(responses::sse_response(responses::sse(vec![
            responses::ev_response_created("step-2"),
            responses::ev_assistant_message("msg-2", "finished on backup without hitting 429"),
            responses::ev_completed("step-2"),
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.update_plan_enabled = true;
            config.account_pool.window_warmup = Some(false);
        })
        .with_pre_build_hook(super::account_failover::write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "run multi-step turn and rotate cleanly between steps".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut switch_warnings = Vec::new();
    loop {
        match fixture.codex.next_event().await?.msg {
            EventMsg::Warning(warning) if warning.message.contains("Switched to Codex account") => {
                switch_warnings.push(warning.message);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    assert_eq!(switch_warnings.len(), 1, "{switch_warnings:?}");
    assert!(switch_warnings[0].contains("97%"), "{switch_warnings:?}");
    Ok(())
}
