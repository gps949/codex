//! Native Remote account menus stay outside inference and ordinary turn state.

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ServerRequestResolvedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const PROFILE_ID: &str = "native-fixture";

#[derive(Clone, Copy)]
enum PoolFixture {
    Empty,
    ManagedAccount,
}

struct NativeFixture {
    app: TestAppServer,
    backend: MockServer,
    home: TempDir,
    thread_id: String,
}

impl NativeFixture {
    async fn new(pool: PoolFixture) -> Result<Self> {
        let home = TempDir::new()?;
        let backend = MockServer::start().await;
        app_test_support::mount_workspace_routing(&backend).await;
        MockResponsesConfig::new(&backend.uri())
            .with_root_config(&format!("chatgpt_base_url = {:?}", backend.uri()))
            .with_extra_config("[account_pool]\nwindow_warmup = false\n")
            .write(home.path())?;
        write_models_cache(home.path()).await?;
        if matches!(pool, PoolFixture::ManagedAccount) {
            let credential_home = home.path().join("auth-profiles").join(PROFILE_ID);
            std::fs::create_dir_all(&credential_home)?;
            write_chatgpt_auth(
                &credential_home,
                ChatGptAuthFixture::new("native-fixture-access")
                    .account_id("native-fixture-workspace")
                    .chatgpt_account_id("native-fixture-workspace")
                    .chatgpt_user_id("native-fixture-user")
                    .email("native-fixture@example.com")
                    .plan_type("pro"),
                AuthCredentialsStoreMode::File,
            )?;
            std::fs::write(
                home.path().join("account-profiles.json"),
                serde_json::to_vec(&json!({
                    "version": 1,
                    "profiles": [{
                        "id": PROFILE_ID, "label": "Work fixture", "priority": 0,
                        "credential_location": "managed_profile", "state": "ready",
                        "disabled": false
                    }]
                }))?,
            )?;
            std::fs::write(
                home.path().join("account-runtime-state.json"),
                serde_json::to_vec(&json!({
                    "version": 1, "active_profile_id": PROFILE_ID, "profiles": []
                }))?,
            )?;
        }
        let mut app = TestAppServer::builder()
            .with_codex_home(home.path())
            .with_env_overrides(&[("OPENAI_API_KEY", None)])
            .build()
            .await?;
        app.initialize_with_capabilities(
            ClientInfo {
                name: "codex_chatgpt_ios_remote".into(),
                title: None,
                version: "native-fixture-version".into(),
            },
            Some(InitializeCapabilities {
                experimental_api: false,
                extensions: Some(HashMap::from([(
                    "io.modelcontextprotocol/ui".into(),
                    json!({
                        "mimeTypes": ["text/x-dil;profile=mcp-app"],
                        "privateFixture": "must-not-appear-in-diagnostics"
                    }),
                )])),
                ..Default::default()
            }),
        )
        .await?;
        // Explicit environment selection is experimental. This client exercises the
        // native menu without that capability and uses the host default environment.
        let request_id = app
            .send_thread_start_request(ThreadStartParams::default())
            .await?;
        let started: ThreadStartResponse =
            timeout(READ_TIMEOUT, app.read_response(request_id)).await??;
        app.clear_message_buffer();
        Ok(Self {
            home,
            backend,
            app,
            thread_id: started.thread.id,
        })
    }

    async fn start(&mut self, text: &str) -> Result<TurnStartResponse> {
        let request_id = self
            .app
            .send_raw_request(
                "turn/start",
                Some(json!({
                    "threadId": self.thread_id,
                    "input": [{"type": "text", "text": text, "textElements": []}]
                })),
            )
            .await?;
        timeout(READ_TIMEOUT, self.app.read_response(request_id)).await?
    }

    async fn open_menu(
        &mut self,
    ) -> Result<(TurnStartResponse, RequestId, ToolRequestUserInputParams)> {
        let started = self.start("/account manage").await?;
        assert_eq!(started.turn.status, TurnStatus::InProgress);
        let (request_id, params) = self.read_question().await?;
        assert_eq!(
            (
                &params.thread_id,
                &params.turn_id,
                params.questions.len(),
                params.is_blocking
            ),
            (&self.thread_id, &started.turn.id, 1, true)
        );
        Ok((started, request_id, params))
    }

    async fn read_question(&mut self) -> Result<(RequestId, ToolRequestUserInputParams)> {
        let request = timeout(READ_TIMEOUT, self.app.read_stream_until_request_message()).await??;
        let ServerRequest::ToolRequestUserInput { request_id, params } = request else {
            anyhow::bail!("expected native question, got {request:?}");
        };
        Ok((request_id, params))
    }

    async fn answer(
        &mut self,
        request_id: RequestId,
        question_id: &str,
        answers: Vec<String>,
    ) -> Result<()> {
        self.app
            .send_response(
                request_id,
                json!({"answers": {question_id: {"answers": answers}}}),
            )
            .await
    }

    async fn choose(
        &mut self,
        request_id: RequestId,
        params: &ToolRequestUserInputParams,
        label: &str,
    ) -> Result<(RequestId, ToolRequestUserInputParams)> {
        assert!(
            params.questions[0]
                .options
                .as_ref()
                .is_some_and(|options| options.iter().any(|option| option.label == label)),
            "captured option {label}"
        );
        self.answer(request_id, &params.questions[0].id, vec![label.into()])
            .await?;
        self.read_question().await
    }

    async fn account_actions(
        &mut self,
    ) -> Result<(TurnStartResponse, RequestId, ToolRequestUserInputParams)> {
        let (turn, id, question) = self.open_menu().await?;
        let (id, question) = self.choose(id, &question, "Accounts").await?;
        let (id, question) = self.choose(id, &question, "1. Work fixture").await?;
        let (id, question) = self.choose(id, &question, "Account actions").await?;
        Ok((turn, id, question))
    }

    async fn finish_menu(
        &mut self,
        turn_id: &str,
        request_id: &RequestId,
    ) -> Result<TurnCompletedNotification> {
        let mut resolved = false;
        loop {
            let message = timeout(READ_TIMEOUT, self.app.read_next_message()).await??;
            let JSONRPCMessage::Notification(notification) = message else {
                continue;
            };
            let params = notification
                .params
                .context("native lifecycle notification params")?;
            match notification.method.as_str() {
                "serverRequest/resolved" => {
                    let notice: ServerRequestResolvedNotification = serde_json::from_value(params)?;
                    if &notice.request_id == request_id {
                        assert_eq!(notice.thread_id, self.thread_id);
                        resolved = true;
                    }
                }
                "turn/completed" => {
                    let notice: TurnCompletedNotification = serde_json::from_value(params)?;
                    if notice.turn.id == turn_id {
                        assert!(
                            resolved,
                            "native callback must resolve before its turn completes"
                        );
                        assert_eq!(notice.thread_id, self.thread_id);
                        return Ok(notice);
                    }
                }
                _ => {}
            }
        }
    }

    async fn inference_requests(&self) -> Vec<wiremock::Request> {
        self.backend
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path().ends_with("/responses"))
            .collect()
    }
}

fn agent_text(items: &[ThreadItem]) -> String {
    items
        .iter()
        .filter_map(|item| match item {
            ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_manager_overview_details_and_close_keep_accounts_and_inference_unchanged()
-> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let paths = [
        fixture.home.path().join("config.toml"),
        fixture.home.path().join("account-profiles.json"),
        fixture
            .home
            .path()
            .join("auth-profiles")
            .join(PROFILE_ID)
            .join("auth.json"),
    ];
    let before = paths
        .iter()
        .map(std::fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let (started, request_id, params) = fixture.open_menu().await?;
    let question = &params.questions[0];
    assert!(
        question
            .options
            .as_ref()
            .context("native options")?
            .iter()
            .any(|option| option.label == "Close")
    );
    fixture
        .answer(request_id.clone(), &question.id, vec!["Accounts".into()])
        .await?;
    let (overview_id, overview) = fixture.read_question().await?;
    assert_ne!(overview_id, request_id);
    assert_ne!(overview.questions[0].id, question.id);
    assert!(!overview.questions[0].question.contains('\n'));
    assert!(
        overview.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| option.label == "1. Work fixture")
    );
    fixture
        .answer(
            overview_id,
            &overview.questions[0].id,
            vec!["1. Work fixture".into()],
        )
        .await?;
    let (details_id, details) = fixture.read_question().await?;
    assert_eq!(details.questions[0].question, "Work fixture");
    fixture
        .answer(
            details_id,
            &details.questions[0].id,
            vec!["Quota and identity".into()],
        )
        .await?;
    let (details_id, details) = fixture.read_question().await?;
    assert!(
        details.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .any(|option| option.description.contains("native-fixture@example.com"))
    );
    fixture
        .answer(details_id, &details.questions[0].id, vec!["Back".into()])
        .await?;
    let (details_id, details) = fixture.read_question().await?;
    fixture
        .answer(
            details_id.clone(),
            &details.questions[0].id,
            vec!["Close".into()],
        )
        .await?;
    let completed = fixture.finish_menu(&started.turn.id, &details_id).await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(
        paths
            .iter()
            .map(std::fs::read)
            .collect::<std::io::Result<Vec<_>>>()?,
        before
    );
    assert!(fixture.inference_requests().await.is_empty());
    let runtime: Value = serde_json::from_slice(&std::fs::read(
        fixture.home.path().join("account-runtime-state.json"),
    )?)?;
    assert_eq!(runtime["active_profile_id"], json!(PROFILE_ID));
    Ok(())
}

#[test_case::test_case(vec!["Close".into(), "user_note: use a credit".into()]; "additional_note")]
#[test_case::test_case(vec!["use a credit".into()]; "free_text")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_manager_rejects_uncaptured_answer_actions(
    answers: Vec<String>,
) -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    let (started, request_id, params) = fixture.open_menu().await?;
    fixture
        .answer(request_id.clone(), &params.questions[0].id, answers)
        .await?;
    fixture.finish_menu(&started.turn.id, &request_id).await?;
    let capabilities = fixture.start("/account capabilities").await?;
    assert!(agent_text(&capabilities.turn.items).contains("Answer format was not accepted"));
    assert!(fixture.inference_requests().await.is_empty());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_manager_stop_ignores_late_answer_and_accepts_next_turn() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(
            core_test_support::responses::sse_response(
                create_final_assistant_message_sse_response("continued after Stop")?,
            )
            .set_delay(Duration::from_secs(1)),
        )
        .expect(1)
        .mount(&fixture.backend)
        .await;
    let (started, request_id, params) = fixture.open_menu().await?;
    let interrupt_id = fixture
        .app
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: fixture.thread_id.clone(),
            turn_id: started.turn.id.clone(),
        })
        .await?;
    let _: TurnInterruptResponse =
        timeout(READ_TIMEOUT, fixture.app.read_response(interrupt_id)).await??;
    let completed = fixture.finish_menu(&started.turn.id, &request_id).await?;
    assert_eq!(completed.turn.status, TurnStatus::Interrupted);
    fixture.app.clear_message_buffer();
    let next = fixture.start("continue after Stop").await?;
    assert_ne!(next.turn.id, started.turn.id);
    fixture
        .answer(request_id, &params.questions[0].id, vec!["Close".into()])
        .await?;
    let real_completed: TurnCompletedNotification = timeout(
        READ_TIMEOUT,
        fixture.app.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(
        (real_completed.turn.id, real_completed.turn.status),
        (next.turn.id, TurnStatus::Completed)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_manager_new_input_finishes_menu_before_real_turn_starts() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::ManagedAccount).await?;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(core_test_support::responses::sse_response(
            create_final_assistant_message_sse_response("ordinary task done")?,
        ))
        .expect(1)
        .mount(&fixture.backend)
        .await;
    let (menu, menu_request, _) = fixture.open_menu().await?;
    fixture.app.clear_message_buffer();
    let real = fixture.start("continue ordinary task").await?;
    let mut menu_resolved = false;
    let mut menu_completed = false;
    let mut real_started = false;
    loop {
        let message = timeout(READ_TIMEOUT, fixture.app.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        let params = notification.params.context("turn lifecycle parameters")?;
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let notice: ServerRequestResolvedNotification = serde_json::from_value(params)?;
                if notice.request_id == menu_request {
                    menu_resolved = true;
                }
            }
            "turn/started" => {
                let notice: TurnStartedNotification = serde_json::from_value(params)?;
                if notice.turn.id == real.turn.id {
                    assert!(
                        menu_completed,
                        "synthetic completion must precede the next real turn"
                    );
                    real_started = true;
                }
            }
            "turn/completed" => {
                let notice: TurnCompletedNotification = serde_json::from_value(params)?;
                if notice.turn.id == menu.turn.id {
                    assert!(menu_resolved);
                    assert!(
                        !real_started,
                        "delayed synthetic completion can clear the new mobile turn UI"
                    );
                    menu_completed = true;
                } else if notice.turn.id == real.turn.id {
                    assert!(real_started);
                    assert_eq!(notice.turn.status, TurnStatus::Completed);
                    break;
                }
            }
            _ => {}
        }
    }
    let requests = fixture.inference_requests().await;
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    let model_input = serde_json::to_string(&body["input"])?;
    assert!(model_input.contains("continue ordinary task"));
    assert!(!model_input.contains("/account manage"));
    assert!(!model_input.contains(&menu.turn.id));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_capabilities_works_without_pool_and_hides_raw_extensions() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::Empty).await?;
    let reply = fixture.start("/account capabilities").await?;
    assert_eq!(reply.turn.status, TurnStatus::Completed);
    let text = agent_text(&reply.turn.items);
    assert!(text.contains("codex_chatgpt_ios_remote"));
    assert!(text.contains("Not tested on this connection"));
    assert!(text.contains("text/x-dil;profile=mcp-app"));
    assert!(!text.contains("must-not-appear-in-diagnostics"));
    assert!(fixture.inference_requests().await.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_account_menu_finishes_before_inline_review_and_startup_stop() -> Result<()> {
    let mut fixture = NativeFixture::new(PoolFixture::Empty).await?;
    let (menu, request, _) = fixture.open_menu().await?;
    let id = fixture
        .app
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: fixture.thread_id.clone(),
            turn_id: String::new(),
        })
        .await?;
    let _: TurnInterruptResponse = timeout(READ_TIMEOUT, fixture.app.read_response(id)).await??;
    let stopped = fixture.finish_menu(&menu.turn.id, &request).await?;
    assert_eq!(stopped.turn.status, TurnStatus::Interrupted);

    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(core_test_support::responses::sse_response(
            create_final_assistant_message_sse_response("review done")?,
        ))
        .mount(&fixture.backend)
        .await;
    let (menu, menu_request, _) = fixture.open_menu().await?;
    let id = fixture
        .app
        .send_review_start_request(codex_app_server_protocol::ReviewStartParams {
            thread_id: fixture.thread_id.clone(),
            delivery: Some(codex_app_server_protocol::ReviewDelivery::Inline),
            target: codex_app_server_protocol::ReviewTarget::Custom {
                instructions: "Review current task".into(),
            },
        })
        .await?;
    let _: codex_app_server_protocol::ReviewStartResponse =
        timeout(READ_TIMEOUT, fixture.app.read_response(id)).await??;
    let mut resolved = false;
    let mut completed = false;
    loop {
        let message = timeout(READ_TIMEOUT, fixture.app.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        let params = notification.params.context("review lifecycle params")?;
        match notification.method.as_str() {
            "serverRequest/resolved" => {
                let notice: ServerRequestResolvedNotification = serde_json::from_value(params)?;
                if notice.request_id == menu_request {
                    resolved = true;
                }
            }
            "turn/completed" => {
                let notice: TurnCompletedNotification = serde_json::from_value(params)?;
                if notice.turn.id == menu.turn.id {
                    assert!(resolved);
                    completed = true;
                }
            }
            "turn/started" => {
                let notice: TurnStartedNotification = serde_json::from_value(params)?;
                if notice.turn.id != menu.turn.id {
                    assert!(completed);
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[path = "native_account_manager_actions.rs"]
mod actions;
