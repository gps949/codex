use std::time::Duration;

use codex_config::AutoResetCredits;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::ExecutionAccountPoolHandle;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_login::AccountProfileId;
use codex_login::AccountRuntimeStateStore;
use codex_login::ApiAccount;
use codex_login::ApiAccountFallback;
use codex_login::ApiAccountSelection;
use codex_login::ApiAccountStore;
use codex_login::AuthKeyringBackendKind;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_message_item_added;
use core_test_support::responses::ev_output_text_delta;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::super::account_failover::write_profile_credentials;
use super::collect_turn_events;
use super::message_count;
use super::mount_denied_pool_usage;
use super::quota_exceeded_event;
use super::write_backup_only_account_pool_fixture;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_during_credit_lock_wait_retains_input_without_spending() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    mount_denied_pool_usage(&server, "backup-acct", "access-backup").await;
    let requests = mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
                "type": "usage_limit_reached", "message": "synthetic quota exhaustion",
                "resets_at": chrono::Utc::now().timestamp() + 14_400,
            }})),
            responses::sse_response(sse(vec![
                ev_response_created("steered-recovery"),
                ev_assistant_message("steered-answer", "processed the updated request"),
                ev_completed("steered-recovery"),
            ])),
        ],
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(0)
        .mount(&server)
        .await;
    let backend = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(write_backup_only_account_pool_fixture)
        .with_config(move |config| {
            config.chatgpt_base_url = backend;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
            config.account_pool.auto_reset_credits = Some(AutoResetCredits::WhenPoolExhausted);
            config.account_pool.auto_reset_credit_min_wait_minutes = Some(0);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let store = AccountRuntimeStateStore::new(fixture.home.path().to_path_buf());
    let spend_lock = store
        .try_lock_reset_credit()?
        .expect("own synthetic spending lock");
    let started = fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "continue my original task".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let TurnInputSubmission::Started { turn_id } = started else {
        anyhow::bail!("expected a new recovery turn");
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if server
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .any(|request| request.url.path() == "/backend-api/wham/usage")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    // Let the denied metadata response reach the held spending lock before steering.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let handle = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    handle
        .activate(&AccountProfileId::new("backup-acct")?, /*force*/ true)
        .await?;
    assert_eq!(
        fixture
            .codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "include the correction in that task".into(),
                text_elements: Vec::new(),
            },]))
            .await?,
        TurnInputSubmission::Steered {
            turn_id: turn_id.clone()
        }
    );
    let events =
        tokio::time::timeout(Duration::from_secs(5), collect_turn_events(&fixture.codex)).await??;
    drop(spend_lock);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, EventMsg::Error(_)))
    );
    assert!(events.iter().any(|event| matches!(event, EventMsg::TurnComplete(complete)
        if complete.turn_id == turn_id && complete.last_agent_message.as_deref() == Some("processed the updated request"))));
    let requests = requests.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        (
            message_count(&requests[1], "user", "continue my original task"),
            message_count(&requests[1], "user", "include the correction in that task"),
        ),
        (1, 1)
    );
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exhausted_pool_with_visible_partial_output_does_not_redeem_a_credit() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    mount_denied_pool_usage(&server, "backup-acct", "access-backup").await;
    let requests = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("partial-credit-response"),
            ev_message_item_added("partial-credit-message", ""),
            ev_output_text_delta("already visible"),
            quota_exceeded_event("partial-credit-response"),
        ]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(0)
        .mount(&server)
        .await;
    let backend = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(write_backup_only_account_pool_fixture)
        .with_config(move |config| {
            config.chatgpt_base_url = backend;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
            config.account_pool.auto_reset_credits = Some(AutoResetCredits::WhenPoolExhausted);
            config.account_pool.auto_reset_credit_min_wait_minutes = Some(0);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "preserve credits when partial output prevents safe replay".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let events =
        tokio::time::timeout(Duration::from_secs(5), collect_turn_events(&fixture.codex)).await??;
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                EventMsg::AgentMessageContentDelta(delta) => Some(delta.delta.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["already visible"]
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                EventMsg::Error(error) => error.codex_error_info.clone(),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![CodexErrorInfo::UsageLimitExceeded]
    );
    assert_eq!(requests.requests().len(), 1);
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fifth_seat_metadata_recovery_prevents_paid_api_fallback() -> anyhow::Result<()> {
    let subscription = MockServer::start().await;
    let api = MockServer::start().await;
    for seat in ["seat-0", "seat-1", "seat-2", "seat-3"] {
        mount_denied_pool_usage(&subscription, seat, &format!("access-{seat}")).await;
    }
    let reset = chrono::Utc::now() + chrono::Duration::hours(4);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(header("authorization", "Bearer access-seat-4"))
        .and(header("chatgpt-account-id", "account-seat-4"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "plan_type": "pro", "account_id": "account-seat-4", "user_id": "user-seat-4",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 0, "limit_window_seconds": 18000,
                    "reset_after_seconds": 14400, "reset_at": reset.timestamp()},
                "secondary_window": {"used_percent": 0, "limit_window_seconds": 604800,
                    "reset_after_seconds": 14400, "reset_at": reset.timestamp()}},
            "spend_control": {"reached": false},
        })))
        .expect(1)
        .mount(&subscription)
        .await;
    let requests = mount_sse_once(
        &subscription,
        sse(vec![
            ev_response_created("fifth-seat-response"),
            ev_assistant_message(
                "fifth-seat-answer",
                "finished using free subscription quota",
            ),
            ev_completed("fifth-seat-response"),
        ]),
    )
    .await;
    let api_url = api.uri();
    let backend = format!("{}/backend-api", subscription.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(move |home| {
            let mut profiles = Vec::new();
            let mut runtime = Vec::new();
            for index in 0..5 {
                let id = format!("seat-{index}");
                write_profile_credentials(home, &id, &format!("access-{id}"));
                profiles.push(json!({"id": id, "label": null, "priority": index,
                    "credential_location": "managed_profile", "state": "ready", "disabled": false}));
                let window = json!({"used_percent": 100.0, "resets_at": reset, "window_minutes": 300});
                runtime.push(json!({"profile_id": id, "exhausted_until": reset,
                    "backend_resets_at": reset,
                    "rate_limits": {"primary": window, "secondary": window, "observed_at": chrono::Utc::now()}}));
            }
            std::fs::write(home.join("account-profiles.json"), serde_json::to_vec(&json!({
                "version": 1, "profiles": profiles,
            })).expect("encode synthetic profiles")).expect("write synthetic profiles");
            std::fs::write(home.join("account-runtime-state.json"), serde_json::to_vec(&json!({
                "version": 1, "active_profile_id": "seat-0", "profiles": runtime,
            })).expect("encode exhausted fixture")).expect("write exhausted fixture");
            let api_store = ApiAccountStore::new(home.to_path_buf(), AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default());
            let account = api_store.add(ApiAccount {
                id: String::new(), label: "Synthetic paid fallback".into(), base_url: api_url,
                model: "vendor-exact-model".into(), disabled: false, context_window: 32_768, images: false,
            }, "synthetic-api-key").expect("add synthetic fallback");
            api_store.configure_fallback(ApiAccountFallback {
                enabled: true, profile_id: Some(account.id), wait_minutes: 0,
            }).expect("enable synthetic fallback");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = backend;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
            config.account_pool.auto_reset_credits = Some(AutoResetCredits::Never);
        });
    let fixture = builder.build_with_auto_env(&subscription).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "check every free seat before paid fallback".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let events = tokio::time::timeout(Duration::from_secs(10), collect_turn_events(&fixture.codex))
        .await??;
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, EventMsg::Error(_)))
    );
    assert!(events.iter().any(|event| matches!(event, EventMsg::TurnComplete(complete)
        if complete.last_agent_message.as_deref() == Some("finished using free subscription quota"))));
    assert_eq!(
        requests
            .requests()
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![Some("Bearer access-seat-4".into())]
    );
    assert_eq!(api.received_requests().await.unwrap_or_default().len(), 0);
    let state = ApiAccountStore::new(
        fixture.home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .load()?;
    assert_eq!(state.selection, ApiAccountSelection::Subscription);
    let mut usage_profiles = subscription
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.url.path() == "/backend-api/wham/usage")
        .map(|request| {
            request
                .headers
                .get("chatgpt-account-id")
                .expect("profile routing header")
                .to_str()
                .expect("valid synthetic profile ID")
                .to_owned()
        })
        .collect::<Vec<_>>();
    usage_profiles.sort();
    // Discovery stops once a usable subscription responds; other concurrent reads need not finish.
    assert_eq!(
        usage_profiles
            .iter()
            .filter(|profile| profile.as_str() == "account-seat-4")
            .count(),
        1,
        "the restored seat must be checked before resuming subscription inference",
    );
    assert!(
        usage_profiles
            .windows(2)
            .all(|profiles| profiles[0] != profiles[1]),
        "a recovery pass must not recheck an already observed seat",
    );
    assert!(usage_profiles.iter().all(|profile| matches!(
        profile.as_str(),
        "account-seat-0"
            | "account-seat-1"
            | "account-seat-2"
            | "account-seat-3"
            | "account-seat-4"
    )));
    subscription.verify().await;
    api.verify().await;
    Ok(())
}
