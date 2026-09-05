use std::sync::Mutex;
use std::sync::mpsc;

use codex_core::ExecutionAccountPoolHandle;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_login::AccountAvailability;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::assistant_execution_provenance;
use super::account_failover::write_account_pool_fixture;

struct GatedProfileResponder {
    primary_started: mpsc::Sender<()>,
    primary_release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Respond for GatedProfileResponder {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .expect("authorization header");
        if authorization == "Bearer access-primary" {
            self.primary_started
                .send(())
                .expect("signal primary request capture");
            let release = self
                .primary_release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("primary release receiver");
            release.recv().expect("release primary response");
            return ResponseTemplate::new(/*status*/ 429).set_body_json(json!({
                "error": {
                    "type": "usage_limit_reached",
                    "message": "late primary usage limit",
                    "resets_at": chrono::Utc::now().timestamp() + 3600,
                },
            }));
        }
        core_test_support::responses::sse_response(sse(vec![
            ev_response_created("continued-response"),
            ev_assistant_message("continued-message", "PRIMARY_CONTINUED_ON_BACKUP"),
            ev_completed("continued-response"),
        ]))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_threads_share_the_pool_but_keep_request_auth_and_provenance_bound()
-> anyhow::Result<()> {
    let primary_server = MockServer::start().await;
    let second_primary_server = MockServer::start().await;
    let backup_server = MockServer::start().await;
    let (primary_started_tx, primary_started_rx) = mpsc::channel();
    let (primary_release_tx, primary_release_rx) = mpsc::channel();
    let (second_primary_started_tx, second_primary_started_rx) = mpsc::channel();
    let (second_primary_release_tx, second_primary_release_rx) = mpsc::channel();
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(GatedProfileResponder {
            primary_started: primary_started_tx,
            primary_release: Mutex::new(Some(primary_release_rx)),
        })
        .expect(/*requests*/ 2)
        .mount(&primary_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(GatedProfileResponder {
            primary_started: second_primary_started_tx,
            primary_release: Mutex::new(Some(second_primary_release_rx)),
        })
        .expect(/*requests*/ 2)
        .mount(&second_primary_server)
        .await;
    let backup_response = mount_sse_once(
        &backup_server,
        sse(vec![
            ev_response_created("backup-response"),
            ev_assistant_message("backup-message", "BACKUP_BOUND_REPLY"),
            ev_completed("backup-response"),
        ]),
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&primary_server).await?;

    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "primary concurrent turn".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let mut second_primary_config = fixture.config.clone();
    second_primary_config.model_provider.base_url =
        Some(format!("{}/v1", second_primary_server.uri()));
    let second_primary_options = StartThreadOptions {
        session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id: fixture.session_configured.thread_id,
            depth: 1,
            agent_path: None,
            agent_nickname: Some("request-auth-test".to_string()),
            agent_role: None,
        })),
        ..StartThreadOptions::new(second_primary_config)
    };
    let second_primary_thread = fixture
        .thread_manager
        .start_thread(second_primary_options)
        .await?;
    second_primary_thread
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "second primary concurrent turn".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::task::spawn_blocking(move || -> Result<(), mpsc::RecvError> {
        primary_started_rx.recv()?;
        second_primary_started_rx.recv()?;
        Ok(())
    })
    .await??;

    let pool = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    assert_eq!(
        pool.active_identity()
            .expect("primary active identity")
            .profile_id
            .to_string(),
        "primary-acct",
    );
    let backup_id = codex_login::AccountProfileId::new("backup-acct")?;
    let backup_identity = pool.activate(&backup_id, /*force*/ false).await?;
    let mut backup_config = fixture.config.clone();
    backup_config.model_provider.base_url = Some(format!("{}/v1", backup_server.uri()));
    let backup_thread = fixture
        .thread_manager
        .start_thread(StartThreadOptions::new(backup_config))
        .await?;
    backup_thread
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "backup concurrent turn".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&backup_thread.thread, |msg| {
        matches!(msg, EventMsg::TurnComplete(_))
    })
    .await;
    primary_release_tx.send(())?;
    second_primary_release_tx.send(())?;
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
    assert_eq!(switch_warnings, Vec::<String>::new());
    let mut second_switch_warnings = Vec::new();
    loop {
        match second_primary_thread.thread.next_event().await?.msg {
            EventMsg::Warning(warning) if warning.message.contains("switched to") => {
                second_switch_warnings.push(warning.message);
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    assert_eq!(second_switch_warnings, Vec::<String>::new());

    let primary_requests = primary_server
        .received_requests()
        .await
        .expect("captured primary request")
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .collect::<Vec<_>>();
    let second_primary_requests = second_primary_server
        .received_requests()
        .await
        .expect("captured second primary request")
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .collect::<Vec<_>>();
    assert_eq!(
        (primary_requests.len(), second_primary_requests.len()),
        (2, 2)
    );
    let backup_request = backup_response.single_request();
    assert_eq!(
        (
            (
                primary_requests[0]
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                primary_requests[0]
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
            ),
            (
                primary_requests[1]
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                primary_requests[1]
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
            ),
            (
                second_primary_requests[0]
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                second_primary_requests[0]
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
            ),
            (
                second_primary_requests[1]
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                second_primary_requests[1]
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok()),
            ),
            backup_request.header("authorization"),
        ),
        (
            (Some("Bearer access-primary"), Some("account-primary-acct")),
            (Some("Bearer access-backup"), Some("account-backup-acct")),
            (Some("Bearer access-primary"), Some("account-primary-acct")),
            (Some("Bearer access-backup"), Some("account-backup-acct")),
            Some("Bearer access-backup".to_string()),
        ),
    );
    assert_eq!(
        assistant_execution_provenance(
            second_primary_thread
                .session_configured
                .rollout_path
                .as_deref()
                .expect("second primary rollout path"),
            "PRIMARY_CONTINUED_ON_BACKUP",
        )?,
        (
            Some(backup_identity.profile_id.to_string()),
            Some(backup_identity.generation),
        ),
    );
    assert_eq!(
        assistant_execution_provenance(
            fixture
                .session_configured
                .rollout_path
                .as_deref()
                .expect("primary rollout path"),
            "PRIMARY_CONTINUED_ON_BACKUP",
        )?,
        (
            Some(backup_identity.profile_id.to_string()),
            Some(backup_identity.generation),
        ),
    );
    assert_eq!(
        assistant_execution_provenance(
            backup_thread
                .session_configured
                .rollout_path
                .as_deref()
                .expect("backup rollout path"),
            "BACKUP_BOUND_REPLY",
        )?,
        (
            Some(backup_identity.profile_id.to_string()),
            Some(backup_identity.generation),
        ),
    );
    let snapshots = pool.snapshots();
    assert!(matches!(
        snapshots[0].availability,
        AccountAvailability::Exhausted { .. }
    ));
    assert_eq!(
        pool.active_identity(),
        Some(backup_identity),
        "late primary failure must not rotate or regenerate the active backup identity",
    );
    Ok(())
}
