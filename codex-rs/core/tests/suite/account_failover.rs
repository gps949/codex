use std::path::Path;
use std::sync::Arc;

use codex_core::ExecutionAccountPoolHandle;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_login::AccountAvailability;
use codex_login::CodexAuth;
use codex_login::ExternalAuth;
use codex_login::ExternalAuthFuture;
use codex_login::ExternalAuthRefreshContext;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
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

struct StaticManagedAuth {
    auth: CodexAuth,
}

impl ExternalAuth for StaticManagedAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.auth.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.auth.clone()) })
    }
}

/// Writes one ready managed account profile: fake ChatGPT credentials in the profile's
/// credential home plus its manifest entry. Mirrors the on-disk layout produced by
/// `codex account add`.
fn profile_manifest_entry(id: &str, priority: u32) -> serde_json::Value {
    json!({
        "id": id,
        "label": null,
        "priority": priority,
        "credential_location": "managed_profile",
        "state": "ready",
        "disabled": false,
    })
}

#[expect(clippy::unwrap_used)]
fn write_profile_credentials(codex_home: &Path, id: &str, access_token: &str) {
    use base64::Engine as _;

    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(&serde_json::to_vec(&json!({ "alg": "none", "typ": "JWT" })).unwrap());
    let payload = b64(&serde_json::to_vec(&json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": format!("account-{id}"),
        }
    }))
    .unwrap());
    let fake_jwt = format!("{header}.{payload}.{}", b64(b"sig"));

    let credential_home = codex_home.join("auth-profiles").join(id);
    std::fs::create_dir_all(&credential_home).unwrap();
    std::fs::write(
        credential_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": access_token,
                "refresh_token": format!("refresh-{id}"),
                "account_id": format!("account-{id}"),
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .unwrap(),
    )
    .unwrap();
}

#[expect(clippy::unwrap_used)]
pub(super) fn write_account_pool_fixture(codex_home: &Path) {
    write_profile_credentials(codex_home, "primary-acct", "access-primary");
    write_profile_credentials(codex_home, "backup-acct", "access-backup");
    std::fs::write(
        codex_home.join("account-profiles.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "profiles": [
                profile_manifest_entry("primary-acct", 0),
                profile_manifest_entry("backup-acct", 10),
            ],
        }))
        .unwrap(),
    )
    .unwrap();
}

#[expect(clippy::unwrap_used)]
fn write_malformed_account_pool_fixture(codex_home: &Path) {
    std::fs::write(
        codex_home.join("account-profiles.json"),
        "not valid account profile json",
    )
    .unwrap();
}

#[expect(clippy::unwrap_used)]
fn write_backup_only_account_pool_fixture(codex_home: &Path) {
    write_profile_credentials(codex_home, "backup-acct", "access-backup");
    std::fs::write(
        codex_home.join("account-profiles.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "profiles": [profile_manifest_entry("backup-acct", 10)],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn stamp_resumed_model_items_as_profile_a(rollout_path: &Path) -> anyhow::Result<()> {
    let mut lines = std::fs::read_to_string(rollout_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?;
    for line in &mut lines {
        let RolloutItem::ResponseItem(envelope) = &mut line.item else {
            continue;
        };
        let model_owned = match &envelope.item {
            ResponseItem::Reasoning { .. } => true,
            ResponseItem::Message { role, .. } => role == "assistant",
            _ => false,
        };
        if model_owned {
            let metadata = envelope
                .metadata
                .get_or_insert_with(CodexHarnessMetadata::default);
            metadata.execution_profile_id = Some("primary-acct".to_string());
            metadata.execution_generation = Some(7);
        }
    }
    let rewritten = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    std::fs::write(rollout_path, format!("{rewritten}\n"))?;
    Ok(())
}

pub(super) fn assistant_execution_provenance(
    rollout_path: &Path,
    expected_text: &str,
) -> anyhow::Result<(Option<String>, Option<u64>)> {
    let envelope = std::fs::read_to_string(rollout_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find_map(|line| match line.item {
            RolloutItem::ResponseItem(envelope)
                if matches!(
                    &envelope.item,
                    ResponseItem::Message { role, content, .. }
                        if role == "assistant"
                            && content.iter().any(|item| {
                                matches!(item, ContentItem::OutputText { text } if text == expected_text)
                            })
                ) => Some(envelope),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("assistant response {expected_text:?} missing from rollout"))?;
    let metadata = envelope
        .metadata
        .ok_or_else(|| anyhow::anyhow!("assistant response is missing execution provenance"))?;
    Ok((metadata.execution_profile_id, metadata.execution_generation))
}

fn response_item_execution_provenance(
    rollout_path: &Path,
    predicate: impl Fn(&ResponseItem) -> bool,
) -> anyhow::Result<(Option<String>, Option<u64>)> {
    let envelope = std::fs::read_to_string(rollout_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find_map(|line| match line.item {
            RolloutItem::ResponseItem(envelope) if predicate(&envelope.item) => Some(envelope),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("matching response item missing from rollout"))?;
    let metadata = envelope
        .metadata
        .ok_or_else(|| anyhow::anyhow!("matching response item is missing execution provenance"))?;
    Ok((metadata.execution_profile_id, metadata.execution_generation))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_transition_respond_to_model_output_keeps_execution_provenance()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    // The legacy RespondToModel output has an empty call id. Pair it with a completed local-shell
    // item and use a raw responder so request-shape validation cannot mask the provenance check.
    let bodies = [
        sse(vec![
            ev_response_created("malformed-tool-search-response"),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "local_shell_call",
                    "call_id": "",
                    "status": "completed",
                    "action": {
                        "type": "exec",
                        "command": ["true"],
                    },
                },
            }),
            core_test_support::responses::ev_tool_search_call(
                "malformed-search",
                &json!({"limit": 1}),
            ),
            ev_completed("malformed-tool-search-response"),
        ]),
        sse(vec![
            ev_response_created("follow-up-response"),
            ev_assistant_message("follow-up-message", "completed after tool error"),
            ev_completed("follow-up-response"),
        ]),
    ];
    let next_response = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_: &wiremock::Request| {
            let index = next_response.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(bodies[index].clone())
        })
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;

    fixture
        .submit_turn("exercise a rejected tool search")
        .await?;

    let rollout_path = fixture
        .session_configured
        .rollout_path
        .as_deref()
        .expect("account transition rollout path");
    let tool_call_provenance = response_item_execution_provenance(rollout_path, |item| {
        matches!(
            item,
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                ..
            } if call_id == "malformed-search"
        )
    })?;
    let synthesized_output_provenance = response_item_execution_provenance(rollout_path, |item| {
        matches!(
            item,
            ResponseItem::FunctionCallOutput { output, .. }
                if matches!(
                    &output.body,
                    FunctionCallOutputBody::Text(text)
                        if text.contains("failed to parse tool_search arguments")
                )
        )
    })?;
    let active_identity = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager())
        .active_identity()
        .expect("active execution identity");
    let expected = (
        Some(active_identity.profile_id.to_string()),
        Some(active_identity.generation),
    );

    assert_eq!(
        (tool_call_provenance, synthesized_output_provenance),
        (expected.clone(), expected),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_transition_single_profile_resume_projects_foreign_model_state()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("profile-a-response"),
                ev_reasoning_item("reasoning-a", &["portable summary"], &["private details"]),
                ev_assistant_message("message-a", "profile A answer"),
                ev_completed("profile-a-response"),
            ]),
            sse(vec![
                ev_response_created("profile-b-response"),
                ev_completed("profile-b-response"),
            ]),
        ],
    )
    .await;
    let mut initial_builder =
        test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let initial = initial_builder.build_with_auto_env(&server).await?;
    initial.submit_turn("portable before resume").await?;
    let home = Arc::clone(&initial.home);
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("initial rollout path");
    initial.codex.shutdown_and_wait().await?;
    initial.thread_manager.auth_manager().logout().await?;
    stamp_resumed_model_items_as_profile_a(&rollout_path)?;
    write_backup_only_account_pool_fixture(home.path());

    let mut resume_builder = test_codex().without_auth();
    let resumed = resume_builder
        .resume_with_auto_env(&server, home, rollout_path)
        .await?;
    resumed.submit_turn("after B-only resume").await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    let initial_user = requests[0]
        .input()
        .into_iter()
        .find(|item| item["role"] == "user" && item.to_string().contains("portable before resume"))
        .expect("initial portable user input");
    let resumed_input = requests[1].input();
    let resumed_user = resumed_input
        .iter()
        .find(|item| item["role"] == "user" && item.to_string().contains("portable before resume"))
        .expect("resumed portable user input");
    assert_eq!(resumed_user, &initial_user);
    let reasoning = resumed_input
        .iter()
        .find(|item| item["type"] == "reasoning")
        .expect("projected reasoning summary");
    assert!(reasoning.get("id").is_none() || reasoning["id"].is_null());
    assert!(
        reasoning.get("encrypted_content").is_none() || reasoning["encrypted_content"].is_null()
    );
    assert!(reasoning.to_string().contains("portable summary"));
    let assistant = resumed_input
        .iter()
        .find(|item| item["role"] == "assistant")
        .expect("projected assistant message");
    assert!(assistant.get("id").is_none() || assistant["id"].is_null());
    assert!(assistant.to_string().contains("profile A answer"));
    assert_eq!(
        (
            requests[1].header("authorization"),
            requests[1].header("chatgpt-account-id"),
        ),
        (
            Some("Bearer access-backup".to_string()),
            Some("account-backup-acct".to_string()),
        ),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_account_pool_fails_closed_before_sampling() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let unexpected_sampling = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("unexpected-response"),
            ev_assistant_message("unexpected-message", "request should not be sent"),
            ev_completed("unexpected-response"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_malformed_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let terminal = wait_for_event(&fixture.codex, |msg| {
        matches!(msg, EventMsg::Error(_) | EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::Error(error) = terminal else {
        panic!("malformed configured account pool must fail the turn");
    };
    assert!(
        error
            .message
            .contains("failed to initialize native multi-account execution"),
        "unexpected pool initialization error: {}",
        error.message,
    );
    assert!(
        unexpected_sampling.requests().is_empty(),
        "malformed configured account pool must not send a /responses request",
    );

    Ok(())
}

/// End-to-end: a usage-limit rejection on the preferred account rotates the pool to the backup
/// account, warns the user, and completes the same turn on the backup account's credentials.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_rotates_to_backup_account_and_completes_turn() -> anyhow::Result<()> {
    let server = MockServer::start().await;

    // The request-bound token is authoritative even when advisory plan metadata disagrees.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "limit reached",
                "resets_at": chrono::Utc::now().timestamp() + 3600,
                "plan_type": "team"
            }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Replayed request after the account switch: normal completion.
    let success = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "hello from the backup account"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build(&server).await?;
    let codex = fixture.codex.clone();

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let warning = wait_for_event(&codex, |msg| matches!(msg, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning) = warning else {
        unreachable!();
    };
    assert!(
        warning.message.contains("switched to `backup-acct`"),
        "unexpected switch warning: {}",
        warning.message
    );

    wait_for_event(&codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;

    let request_authorizations = server
        .received_requests()
        .await
        .expect("captured response requests")
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
        vec![
            Some("Bearer access-primary".to_string()),
            Some("Bearer access-backup".to_string()),
        ],
        "each request must use the token from its captured execution lease"
    );

    // The replayed request must run on the backup account's credentials.
    let request = success.single_request();
    assert_eq!(
        request.header("authorization"),
        Some("Bearer access-backup".to_string())
    );

    // The scheduler must persist the rotation for future processes.
    let runtime_state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        fixture.home.path().join("account-runtime-state.json"),
    )?)?;
    assert_eq!(
        runtime_state["active_profile_id"],
        json!("backup-acct"),
        "unexpected runtime state: {runtime_state:#}"
    );
    assert_eq!(
        runtime_state["profiles"][0]["profile_id"],
        json!("primary-acct")
    );
    assert!(runtime_state["profiles"][0]["exhausted_until"].is_string());
    assert_eq!(
        runtime_state["profiles"][1]["profile_id"],
        json!("backup-acct")
    );
    assert!(
        runtime_state["profiles"][1]["exhausted_until"].is_null(),
        "the backup profile must remain available: {runtime_state:#}"
    );

    Ok(())
}

#[derive(Clone, Copy)]
enum RemoteCompactTransport {
    Legacy,
    V2,
}

#[test_case::test_case(RemoteCompactTransport::Legacy; "legacy")]
#[test_case::test_case(RemoteCompactTransport::V2; "v2")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pooled_remote_compact_uses_the_captured_profile_auth(
    transport: RemoteCompactTransport,
) -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let seed_response = sse(vec![
        ev_response_created("seed-response"),
        ev_assistant_message("seed-message", "seed history"),
        ev_completed("seed-response"),
    ]);
    let compact = match transport {
        RemoteCompactTransport::Legacy => {
            mount_sse_once(&server, seed_response).await;
            core_test_support::responses::mount_compact_json_once(
                &server,
                json!({
                    "output": [{
                        "type": "compaction",
                        "encrypted_content": "BOUND_REMOTE_COMPACTION",
                    }],
                }),
            )
            .await
        }
        RemoteCompactTransport::V2 => {
            core_test_support::responses::mount_sse_sequence(
                &server,
                vec![
                    seed_response,
                    sse(vec![
                        json!({
                            "type": "response.output_item.done",
                            "item": {
                                "type": "compaction",
                                "encrypted_content": "BOUND_REMOTE_COMPACTION_V2",
                            },
                        }),
                        ev_completed("compact-v2-response"),
                    ]),
                ],
            )
            .await
        }
    };
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture)
        .with_config(move |config| {
            let _ = config.features.set_enabled(
                Feature::RemoteCompactionV2,
                matches!(transport, RemoteCompactTransport::V2),
            );
        });
    let fixture = builder.build_with_auto_env(&server).await?;

    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "seed remote compaction".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&fixture.codex, |msg| {
        matches!(msg, EventMsg::TurnComplete(_))
    })
    .await;
    fixture
        .thread_manager
        .auth_manager()
        .set_external_auth(Arc::new(StaticManagedAuth {
            auth: CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        }))
        .await?;

    fixture.codex.submit(Op::Compact).await?;
    wait_for_event(&fixture.codex, |msg| {
        matches!(msg, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = compact.requests();
    let request = requests.last().expect("compact request");
    assert_eq!(
        (
            request.header("authorization"),
            request.header("chatgpt-account-id"),
        ),
        (
            Some("Bearer access-primary".to_string()),
            Some("account-primary-acct".to_string()),
        ),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stock_provider_ignores_an_exhausted_installed_pool() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "limit reached",
                "resets_at": chrono::Utc::now().timestamp() + 3600,
            }
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "exhaust the managed pool".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&fixture.codex, |msg| matches!(msg, EventMsg::Error(_))).await;
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("captured pool requests")
            .into_iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .count(),
        2,
    );

    fixture
        .thread_manager
        .auth_manager()
        .set_external_auth(Arc::new(StaticManagedAuth {
            auth: CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        }))
        .await?;
    let stock_response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("stock-response"),
            ev_assistant_message("stock-message", "stock completed"),
            ev_completed("stock-response"),
        ]),
    )
    .await;
    let mut stock_config = fixture.config.clone();
    stock_config.model_provider_id = "custom-openai".to_string();
    let stock_thread = fixture
        .thread_manager
        .start_thread(StartThreadOptions::new(stock_config))
        .await?;
    stock_thread
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "run with stock auth".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&stock_thread.thread, |msg| {
        matches!(msg, EventMsg::TurnComplete(_))
    })
    .await;

    let request = stock_response.single_request();
    assert_eq!(
        (
            request.header("authorization"),
            request.header("chatgpt-account-id"),
        ),
        (
            Some("Bearer Access Token".to_string()),
            Some("account_id".to_string()),
        ),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_401_permanent_refresh_failure_fails_over_in_the_turn_loop() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    let primary_auth_path = fixture
        .home
        .path()
        .join("auth-profiles/primary-acct/auth.json");
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(move |_request: &wiremock::Request| {
            let mut auth: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&primary_auth_path).expect("read primary auth"),
            )
            .expect("parse primary auth");
            auth["tokens"]["account_id"] = json!("mismatched-primary-account");
            std::fs::write(
                &primary_auth_path,
                serde_json::to_string_pretty(&auth).expect("serialize mismatched primary auth"),
            )
            .expect("rewrite primary auth");
            ResponseTemplate::new(/*status*/ 401).set_body_json(json!({
                "error": {"message": "primary auth is permanently invalid"}
            }))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let success = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("backup-after-401"),
            ev_assistant_message("backup-message-after-401", "backup completed"),
            ev_completed("backup-after-401"),
        ]),
    )
    .await;

    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "recover through the turn loop".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let mut switch_warnings = Vec::new();
    loop {
        match fixture.codex.next_event().await?.msg {
            EventMsg::Warning(warning) if warning.message.contains("switched to") => {
                switch_warnings.push(warning.message);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    assert_eq!(switch_warnings.len(), 1);
    assert!(switch_warnings[0].contains("backup-acct"));

    assert_eq!(
        success.single_request().header("authorization"),
        Some("Bearer access-backup".to_string()),
    );
    let pool = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    let active_identity = pool.active_identity().expect("backup active identity");
    let snapshots = pool.snapshots();
    assert!(matches!(
        snapshots[0].availability,
        AccountAvailability::AuthenticationUnavailable { .. }
    ));
    assert!(!snapshots[0].is_active);
    assert_eq!(
        (
            snapshots[1].profile.id.to_string(),
            &snapshots[1].availability,
            snapshots[1].is_active,
        ),
        (
            "backup-acct".to_string(),
            &AccountAvailability::Available,
            true,
        ),
    );
    assert_eq!(
        assistant_execution_provenance(
            fixture
                .session_configured
                .rollout_path
                .as_deref()
                .expect("401 rollout path"),
            "backup completed",
        )?,
        (
            Some(active_identity.profile_id.to_string()),
            Some(active_identity.generation),
        ),
    );
    Ok(())
}
