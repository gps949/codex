use codex_config::AutoResetCredits;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_login::CodexAuth;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_message_item_added;
use core_test_support::responses::ev_output_text_delta;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_sequence;
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

use super::account_failover::write_account_pool_fixture;
use super::account_failover::write_backup_only_account_pool_fixture;

fn quota_exceeded_event(response_id: &str) -> serde_json::Value {
    json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "error": {
                "code": "insufficient_quota",
                "message": "synthetic quota exhaustion"
            }
        }
    })
}

fn message_count(request: &ResponsesRequest, role: &str, text: &str) -> usize {
    request
        .inputs_of_type("message")
        .into_iter()
        .filter(|item| item["role"].as_str() == Some(role))
        .filter(|item| {
            item["content"].as_array().is_some_and(|content| {
                content
                    .iter()
                    .any(|part| part["text"].as_str() == Some(text))
            })
        })
        .count()
}

async fn collect_turn_events(codex: &codex_core::CodexThread) -> anyhow::Result<Vec<EventMsg>> {
    let mut events = Vec::new();
    loop {
        let event = codex.next_event().await?.msg;
        let complete = matches!(event, EventMsg::TurnComplete(_));
        events.push(event);
        if complete {
            return Ok(events);
        }
    }
}

async fn run_reset_credit_case(
    mode: AutoResetCredits,
    reset_after_minutes: i64,
    consume_response: ResponseTemplate,
) -> anyhow::Result<(usize, usize)> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-backup"))
        .respond_with(ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "single profile exhausted",
                "resets_at": (chrono::Utc::now()
                    + chrono::Duration::minutes(reset_after_minutes))
                    .timestamp(),
            }
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(consume_response)
        .mount(&server)
        .await;
    let backend_base_url = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(write_backup_only_account_pool_fixture)
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.auto_reset_credits = Some(mode);
            config.account_pool.auto_reset_credit_min_wait_minutes = Some(60);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "exercise reset-credit policy".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let events = collect_turn_events(&fixture.codex).await?;
    let consume_count = server
        .received_requests()
        .await
        .expect("captured reset-credit policy requests")
        .into_iter()
        .filter(|request| {
            request.url.path() == "/backend-api/wham/rate-limit-reset-credits/consume"
        })
        .count();
    let success_warnings = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                EventMsg::Warning(warning)
                    if warning.message.contains("Redeemed one rate-limit reset credit")
            )
        })
        .count();
    Ok((consume_count, success_warnings))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_visible_output_rotates_but_requires_manual_resend() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(responses::sse_response(sse(vec![
            ev_response_created("partial-response"),
            ev_message_item_added("partial-message", ""),
            ev_output_text_delta("visible partial"),
            quota_exceeded_event("partial-response"),
        ])))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-backup"))
        .respond_with(responses::sse_response(sse(vec![
            ev_response_created("unexpected-backup-response"),
            ev_assistant_message("unexpected-backup-message", "must not replay"),
            ev_completed("unexpected-backup-response"),
        ])))
        .mount(&server)
        .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "stream before failing over".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut visible_deltas = Vec::new();
    let mut switch_warnings = Vec::new();
    let mut errors = Vec::new();
    loop {
        match fixture.codex.next_event().await?.msg {
            EventMsg::AgentMessageContentDelta(delta) => visible_deltas.push(delta.delta),
            EventMsg::Warning(warning) if warning.message.contains("switched to") => {
                switch_warnings.push(warning.message);
            }
            EventMsg::Error(error) => errors.push(error.codex_error_info),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(visible_deltas, vec!["visible partial".to_string()]);
    assert_eq!(switch_warnings.len(), 1);
    assert!(switch_warnings[0].contains("re-send your message"));
    assert_eq!(errors, vec![Some(CodexErrorInfo::UsageLimitExceeded)]);
    let request_authorizations = server
        .received_requests()
        .await
        .expect("captured requests")
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        request_authorizations,
        vec![Some("Bearer access-primary".to_string())],
        "partial output must block an automatic backup request",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quota_after_durable_assistant_output_continues_from_history() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("durable-response"),
                ev_assistant_message("durable-message", "durable before quota"),
                quota_exceeded_event("durable-response"),
            ]),
            sse(vec![
                ev_response_created("backup-response"),
                ev_assistant_message("backup-message", "continued on backup"),
                ev_completed("backup-response"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "continue after durable output".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut switch_warnings = Vec::new();
    let mut errors = Vec::new();
    loop {
        match fixture.codex.next_event().await?.msg {
            EventMsg::Warning(warning) if warning.message.contains("switched to") => {
                switch_warnings.push(warning.message);
            }
            EventMsg::Error(error) => errors.push(error),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(errors, Vec::new());
    assert_eq!(switch_warnings.len(), 1);
    assert!(switch_warnings[0].contains("continues automatically"));
    let requests = requests.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer access-primary".to_string()),
            Some("Bearer access-backup".to_string()),
        ],
    );
    assert_eq!(
        (
            message_count(&requests[1], "user", "continue after durable output"),
            message_count(&requests[1], "assistant", "durable before quota"),
        ),
        (1, 1),
        "the backup request must continue from canonical durable history",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_side_effect_is_not_repeated_when_follow_up_sampling_fails() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let call_id = "failover-plan-call";
    let plan_arguments = json!({
        "explanation": "Record the failover checkpoint",
        "plan": [{"step": "Run once", "status": "completed"}],
    });
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("tool-response"),
                ev_function_call(call_id, "update_plan", &plan_arguments.to_string()),
                ev_completed("tool-response"),
            ]),
            sse(vec![
                ev_response_created("post-tool-quota-response"),
                quota_exceeded_event("post-tool-quota-response"),
            ]),
            sse(vec![
                ev_response_created("post-tool-backup-response"),
                ev_assistant_message("post-tool-backup-message", "continued after the tool"),
                ev_completed("post-tool-backup-response"),
            ]),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.update_plan_enabled = true;
        })
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "update the plan once".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let mut plan_updates = Vec::new();
    let mut errors = Vec::new();
    loop {
        match fixture.codex.next_event().await?.msg {
            EventMsg::PlanUpdate(update) => plan_updates.push(update),
            EventMsg::Error(error) => errors.push(error),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(errors, Vec::new());
    assert_eq!(
        plan_updates.len(),
        1,
        "the real tool must execute exactly once"
    );
    assert_eq!(
        plan_updates[0].explanation.as_deref(),
        Some("Record the failover checkpoint")
    );
    let requests = requests.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer access-primary".to_string()),
            Some("Bearer access-primary".to_string()),
            Some("Bearer access-backup".to_string()),
        ],
    );
    for request in &requests[1..] {
        let input = request.input();
        assert_eq!(
            (
                input
                    .iter()
                    .filter(|item| {
                        item["type"].as_str() == Some("function_call")
                            && item["call_id"].as_str() == Some(call_id)
                    })
                    .count(),
                input
                    .iter()
                    .filter(|item| {
                        item["type"].as_str() == Some("function_call_output")
                            && item["call_id"].as_str() == Some(call_id)
                    })
                    .count(),
            ),
            (1, 1),
            "continuation requests must reuse one durable tool call/output pair",
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_eligible_target_rejects_next_turn_without_sampling() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let resets_at = chrono::Utc::now().timestamp() + 3600;
    let responses = mount_response_sequence(
        &server,
        vec![
            ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
                "error": {
                    "type": "usage_limit_reached",
                    "message": "primary exhausted",
                    "resets_at": resets_at,
                }
            })),
            ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
                "error": {
                    "type": "usage_limit_reached",
                    "message": "backup exhausted",
                    "resets_at": resets_at,
                }
            })),
        ],
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;

    for (prompt, expected_warning_count) in [
        ("exhaust the pool", 2),
        ("do not sample without an eligible account", 1),
    ] {
        fixture
            .codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: prompt.to_string(),
                text_elements: Vec::new(),
            }]))
            .await?;
        let mut warnings = Vec::new();
        let mut errors = Vec::new();
        loop {
            match fixture.codex.next_event().await?.msg {
                EventMsg::Warning(warning) => warnings.push(warning.message),
                EventMsg::Error(error) => errors.push(error.codex_error_info),
                EventMsg::TurnComplete(_) => break,
                _ => {}
            }
        }
        assert_eq!(errors, vec![Some(CodexErrorInfo::UsageLimitExceeded)]);
        assert_eq!(warnings.len(), expected_warning_count);
        assert!(
            warnings
                .last()
                .is_some_and(|warning| warning.contains("All configured Codex accounts"))
        );
    }

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "the second turn must fail before sampling"
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer access-primary".to_string()),
            Some("Bearer access-backup".to_string()),
        ],
    );

    Ok(())
}

#[derive(Clone, Copy)]
enum ConcurrentResetCreditOutcome {
    Reset,
    NoCredit,
}

#[derive(Debug, PartialEq, Eq)]
struct ConcurrentResetCreditObservation {
    consume_authorizations: Vec<Option<String>>,
    success_warnings: usize,
    response_requests: usize,
}

async fn run_concurrent_reset_credit_case(
    outcome: ConcurrentResetCreditOutcome,
) -> anyhow::Result<ConcurrentResetCreditObservation> {
    let server = MockServer::start().await;
    let reset_at = chrono::Utc::now().timestamp() + 4 * 3600;
    let mut response_templates = vec![
        ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "single profile exhausted",
                "resets_at": reset_at,
            }
        })),
        ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "concurrent single profile exhausted",
                "resets_at": reset_at,
            }
        })),
    ];
    if matches!(outcome, ConcurrentResetCreditOutcome::Reset) {
        response_templates.push(responses::sse_response(sse(vec![
            ev_response_created("rescued-response"),
            ev_assistant_message("rescued-message", "continued after one reset credit"),
            ev_completed("rescued-response"),
        ])));
    }
    // Concurrent turns race: one request may join reset-credit singleflight before sampling.
    // Allow any count in 1..=templates rather than requiring every template slot.
    let max_response_requests = response_templates.len() as u64;
    let responses = {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        use wiremock::Respond;

        struct SeqResponder {
            num_calls: AtomicUsize,
            responses: Vec<ResponseTemplate>,
        }

        impl Respond for SeqResponder {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let call_num = self.num_calls.fetch_add(1, Ordering::SeqCst);
                self.responses
                    .get(call_num)
                    .unwrap_or_else(|| self.responses.last().expect("templates"))
                    .clone()
            }
        }

        let (mock, response_mock) = responses::base_mock();
        mock.respond_with(SeqResponder {
            num_calls: AtomicUsize::new(0),
            responses: response_templates,
        })
        .up_to_n_times(max_response_requests)
        .expect(1..)
        .mount(&server)
        .await;
        response_mock
    };
    let consume_code = match outcome {
        ConcurrentResetCreditOutcome::Reset => "reset",
        ConcurrentResetCreditOutcome::NoCredit => "no_credit",
    };
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_delay(std::time::Duration::from_millis(250))
                .set_body_json(json!({"code": consume_code, "windows_reset": 2})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let backend_base_url = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(write_backup_only_account_pool_fixture)
        .with_config(move |config| {
            config.chatgpt_base_url = backend_base_url;
            config.account_pool.auto_reset_credits = Some(AutoResetCredits::WhenPoolExhausted);
            config.account_pool.auto_reset_credit_min_wait_minutes = Some(60);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let second = fixture
        .thread_manager
        .start_thread(StartThreadOptions::new(fixture.config.clone()))
        .await?;

    let first_start = fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "first concurrent exhaustion".to_string(),
            text_elements: Vec::new(),
        }]));
    let second_start = second
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "second concurrent exhaustion".to_string(),
            text_elements: Vec::new(),
        }]));
    let (first_started, second_started) = tokio::join!(first_start, second_start);
    first_started?;
    second_started?;
    let (first_events, second_events) = tokio::join!(
        collect_turn_events(&fixture.codex),
        collect_turn_events(&second.thread),
    );
    let all_events = first_events?
        .into_iter()
        .chain(second_events?)
        .collect::<Vec<_>>();

    let consume_authorizations = server
        .received_requests()
        .await
        .expect("captured reset-credit requests")
        .into_iter()
        .filter(|request| {
            request.url.path() == "/backend-api/wham/rate-limit-reset-credits/consume"
        })
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    Ok(ConcurrentResetCreditObservation {
        consume_authorizations,
        success_warnings: all_events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    EventMsg::Warning(warning)
                        if warning.message.contains("Redeemed one rate-limit reset credit")
                )
            })
            .count(),
        response_requests: responses.requests().len(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pool_exhaustion_consumes_one_reset_credit_with_failed_profile_auth()
-> anyhow::Result<()> {
    let observed = run_concurrent_reset_credit_case(ConcurrentResetCreditOutcome::Reset).await?;
    assert_eq!(
        observed.consume_authorizations,
        vec![Some("Bearer access-backup".to_string())]
    );
    assert_eq!(observed.success_warnings, 1);
    assert!(
        (2..=3).contains(&observed.response_requests),
        "expected 2..=3 response requests, got {}",
        observed.response_requests
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_non_reset_outcome_is_shared_without_another_consume() -> anyhow::Result<()> {
    let observed = run_concurrent_reset_credit_case(ConcurrentResetCreditOutcome::NoCredit).await?;
    assert_eq!(
        observed.consume_authorizations,
        vec![Some("Bearer access-backup".to_string())]
    );
    assert_eq!(observed.success_warnings, 0);
    assert!(
        (1..=2).contains(&observed.response_requests),
        "expected 1..=2 response requests, got {}",
        observed.response_requests
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_credit_default_and_near_reset_make_no_consume_request() -> anyhow::Result<()> {
    for (mode, reset_after_minutes) in [
        (AutoResetCredits::Never, 240),
        (AutoResetCredits::WhenPoolExhausted, 30),
    ] {
        assert_eq!(
            run_reset_credit_case(
                mode,
                reset_after_minutes,
                ResponseTemplate::new(/*status*/ 200)
                    .set_body_json(json!({"code": "reset", "windows_reset": 2})),
            )
            .await?,
            (0, 0),
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_credit_non_reset_response_never_reports_success() -> anyhow::Result<()> {
    assert_eq!(
        run_reset_credit_case(
            AutoResetCredits::WhenPoolExhausted,
            /*reset_after_minutes*/ 240,
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "no_credit", "windows_reset": 0})),
        )
        .await?,
        (1, 0),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_credit_http_failure_and_timeout_never_report_success() -> anyhow::Result<()> {
    let cases = [
        ResponseTemplate::new(/*status*/ 500).set_body_string("consume failed"),
        ResponseTemplate::new(/*status*/ 200)
            .set_delay(std::time::Duration::from_secs(11))
            .set_body_json(json!({"code": "reset", "windows_reset": 2})),
    ];
    for consume_response in cases {
        assert_eq!(
            run_reset_credit_case(
                AutoResetCredits::WhenPoolExhausted,
                /*reset_after_minutes*/ 240,
                consume_response,
            )
            .await?,
            (1, 0),
        );
    }
    Ok(())
}
