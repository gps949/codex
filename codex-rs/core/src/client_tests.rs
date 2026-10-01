use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::Prompt;
use super::ReasoningSummaryConfig;
use super::UnauthorizedRecoveryExecution;
use super::X_CODEX_INSTALLATION_ID_HEADER;
use super::X_CODEX_PARENT_THREAD_ID_HEADER;
use super::X_CODEX_TURN_METADATA_HEADER;
use super::X_CODEX_WINDOW_ID_HEADER;
use super::X_OPENAI_SUBAGENT_HEADER;
use crate::AttestationContext;
use crate::AttestationProvider;
use crate::GenerateAttestationFuture;
use crate::execution_request_auth::ExecutionRequestAuth;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::MAX_MCP_ATTRIBUTION_BYTES;
use crate::responses_metadata::MCP_ATTRIBUTION_CLIENT_METADATA_KEY;
use crate::test_support::TestCodexResponsesRequestKind;
use crate::test_support::responses_metadata as test_responses_metadata;
use base64::Engine;
use codex_api::AgentIdentityTelemetry;
use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_api::TransportError;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AccountProfileId;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::ExternalAuth;
use codex_login::ExternalAuthFuture;
use codex_login::ExternalAuthRefreshContext;
use codex_login::RefreshTokenError;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_model_provider::BearerAuthProvider;
use codex_model_provider::ModelProvider;
use codex_model_provider::ModelProviderFuture;
use codex_model_provider::ProviderAccountResult;
use codex_model_provider::ProviderAuthRecoveryMessages;
use codex_model_provider::ProviderUnauthorizedRecovery;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_models_manager::manager::SharedModelsManager;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::auth::AuthMode;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::RefreshTokenFailedError;
use codex_protocol::error::RefreshTokenFailedReason;
use codex_protocol::mcp::McpAttribution;
use codex_protocol::mcp::McpAttributionSource;
use codex_protocol::mcp::McpAttributionStatus;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ExecutedToolCall;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::ToolResultMetadata;
use codex_protocol::models::ToolResultSource;
use codex_protocol::models::ToolResultSources;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_rollout_trace::ExecutionStatus;
use codex_rollout_trace::InferenceTraceAttempt;
use codex_rollout_trace::InferenceTraceContext;
use codex_rollout_trace::RawTraceEventPayload;
use codex_rollout_trace::RolloutTrace;
use codex_rollout_trace::TraceWriter;
use codex_rollout_trace::replay_bundle;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const TEST_CHATGPT_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";
const TEST_INSTALLATION_ID: &str = "11111111-1111-4111-8111-111111111111";

fn test_model_client(session_source: SessionSource) -> ModelClient {
    test_model_client_with_thread_id(ThreadId::new(), session_source)
}

fn test_model_client_with_thread_id(
    thread_id: ThreadId,
    session_source: SessionSource,
) -> ModelClient {
    let provider = create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses);
    ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        thread_id,
        provider,
        session_source,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*reasoning_effort_override_enabled*/ false,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        codex_model_provider::WorkspaceRoutingContext::new(
            "https://chatgpt.com/backend-api".into(),
        ),
        Vec::new(),
    )
}

fn test_openai_model_client(
    auth_manager: Arc<AuthManager>,
    base_url: Option<String>,
    session_source: SessionSource,
) -> ModelClient {
    let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    provider.base_url = base_url;
    provider.supports_websockets = false;
    ModelClient::new(
        Some(auth_manager),
        AgentIdentityAuthPolicy::JwtOnly,
        ThreadId::new(),
        provider,
        session_source,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*reasoning_effort_override_enabled*/ false,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        codex_model_provider::WorkspaceRoutingContext::new(
            "https://chatgpt.com/backend-api".into(),
        ),
        Vec::new(),
    )
}

fn test_user_prompt() -> Prompt {
    Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hello".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: "base instructions".to_string(),
            provenance: None,
        },
        ..Default::default()
    }
}

struct RefreshingExternalAuth {
    initial: CodexAuth,
    refreshed: CodexAuth,
}

impl ExternalAuth for RefreshingExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.initial.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.refreshed.clone()) })
    }
}

struct PermanentlyFailingExternalAuth {
    initial: CodexAuth,
}

impl ExternalAuth for PermanentlyFailingExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.initial.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Err(std::io::Error::other("bound profile refresh failed")) })
    }

    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Permanent(RefreshTokenFailedError::new(
            RefreshTokenFailedReason::Other,
            error.to_string(),
        ))
    }
}

#[tokio::test]
async fn bound_401_does_not_retry_with_outer_refreshed_auth() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(/*status*/ 401).set_body_json(json!({
            "error": {"message": "bound profile unauthorized"}
        })))
        .mount(&server)
        .await;

    let outer_manager =
        AuthManager::from_auth_for_testing(CodexAuth::from_api_key("outer-initial"));
    outer_manager
        .set_external_auth(Arc::new(RefreshingExternalAuth {
            initial: CodexAuth::from_api_key("outer-initial"),
            refreshed: CodexAuth::from_api_key("outer-refreshed"),
        }))
        .await?;
    let bound_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("bound-seed"));
    bound_manager
        .set_external_auth(Arc::new(PermanentlyFailingExternalAuth {
            initial: CodexAuth::from_api_key("bound-token"),
        }))
        .await?;
    let client = test_openai_model_client(
        outer_manager,
        Some(format!("{}/v1", server.uri())),
        SessionSource::Cli,
    );
    let mut session = client.new_session();
    session.bind_execution_auth(ExecutionRequestAuth::new(
        /*profile_id*/ None,
        /*generation*/ 4,
        bound_manager,
    ));
    let prompt = test_user_prompt();
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );

    let result = session
        .stream(
            &prompt,
            &test_model_info(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummaryConfig::None,
            /*service_tier*/ None,
            &responses_metadata,
            &InferenceTraceContext::disabled(),
        )
        .await;
    let Err(error) = result else {
        panic!("bound profile refresh failure must reach the turn loop");
    };
    assert!(matches!(
        error.details(),
        CodexErrorDetails::RefreshTokenFailed(_)
    ));

    let requests = server
        .received_requests()
        .await
        .expect("wiremock request history");
    assert_eq!(
        requests.len(),
        1,
        "outer auth must not retry the bound request"
    );
    assert_eq!(
        requests[0]
            .headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer bound-token"),
    );
    Ok(())
}

#[test]
fn websocket_turn_state_is_reused_only_for_the_same_execution_identity() {
    let client = test_model_client(SessionSource::Cli);
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("token"));
    let mut session = client.new_session();
    let first = ExecutionRequestAuth::new(
        Some(AccountProfileId::new("workspace-user-a").expect("profile id")),
        /*generation*/ 1,
        Arc::clone(&manager),
    );
    session.bind_execution_auth(first.clone());
    session
        .turn_state
        .set("sticky-a".to_string())
        .expect("first turn state");

    session.bind_execution_auth(first);
    assert_eq!(
        session.turn_state.get().map(String::as_str),
        Some("sticky-a")
    );
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    session.websocket_session.last_request = Some(
        client
            .build_responses_request(
                &test_user_prompt(),
                &test_model_info(),
                /*effort*/ None,
                ReasoningSummaryConfig::None,
                /*service_tier*/ None,
                &responses_metadata,
                /*include_internal*/ false,
            )
            .expect("cached websocket request"),
    );
    let (_last_response_tx, last_response_rx) = tokio::sync::oneshot::channel();
    session.websocket_session.last_response_rx = Some(last_response_rx);
    session.websocket_session.last_response_from_untraced_warmup = true;
    session
        .websocket_session
        .set_connection_reused(/*connection_reused*/ true);

    session.bind_execution_auth(ExecutionRequestAuth::new(
        Some(AccountProfileId::new("workspace-user-b").expect("profile id")),
        /*generation*/ 1,
        Arc::clone(&manager),
    ));
    assert_eq!(session.turn_state.get(), None);
    assert!(session.websocket_session.last_request.is_none());
    assert!(session.websocket_session.last_response_rx.is_none());
    assert!(!session.websocket_session.last_response_from_untraced_warmup);
    assert!(!session.websocket_session.connection_reused());

    session
        .turn_state
        .set("sticky-b".to_string())
        .expect("second turn state");
    session.bind_execution_auth(ExecutionRequestAuth::new(
        Some(AccountProfileId::new("workspace-user-b").expect("profile id")),
        /*generation*/ 2,
        manager,
    ));
    assert_eq!(session.turn_state.get(), None);
}

#[test]
fn websocket_reconnect_keeps_the_execution_identity_and_sticky_routing() {
    let client = test_model_client(SessionSource::Cli);
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("token"));
    let request_auth = ExecutionRequestAuth::new(
        Some(AccountProfileId::new("same-profile").unwrap()),
        /*generation*/ 3,
        manager,
    );
    let expected_identity = request_auth.identity();
    let mut session = client.new_session();
    session.bind_execution_auth(request_auth.clone());
    session.turn_state.set("sticky-route".to_owned()).unwrap();

    session.websocket_session.reset(Some("connection_closed"));
    session.bind_execution_auth(request_auth);

    assert_eq!(
        (
            session.websocket_session.execution_identity.clone(),
            session.turn_state.get().cloned(),
        ),
        (Some(expected_identity), Some("sticky-route".to_owned())),
    );
}

fn test_model_provider() -> SharedModelProvider {
    test_model_client(SessionSource::Cli).state.provider.clone()
}

#[tokio::test]
async fn bound_client_setup_uses_the_execution_accounts_workspace_route() {
    use codex_login::WorkspaceRouting;
    use codex_login::WorkspaceRoutingRequest;
    use codex_login::WorkspaceRoutingResolver;

    struct BoundRoute;
    impl WorkspaceRoutingResolver for BoundRoute {
        fn resolve(
            &self,
            _request: WorkspaceRoutingRequest,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = std::io::Result<Option<WorkspaceRouting>>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async {
                Ok(Some(WorkspaceRouting {
                    chatgpt_account_id: "account_id".to_owned(),
                    backend_origin: "https://gov.chatgpt.com".to_owned(),
                    account_routing_override: "us_cr".to_owned(),
                }))
            })
        }
    }

    let outer =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let bound =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(BoundRoute);
    outer.set_workspace_routing_resolver(Arc::downgrade(&resolver));
    let client = test_openai_model_client(outer, /*base_url*/ None, SessionSource::Cli);
    let mut session = client.new_session();
    session.bind_execution_auth(ExecutionRequestAuth::new(
        Some(AccountProfileId::new("bound-profile").unwrap()),
        /*generation*/ 7,
        Arc::clone(&bound),
    ));

    let setup = session
        .current_client_setup(super::ClientRouting::Workspace)
        .await
        .unwrap();
    let mut headers = http::HeaderMap::new();
    setup.api_auth.add_auth_headers(&mut headers);
    assert_eq!(
        (
            setup.api_provider.base_url.as_str(),
            setup
                .api_provider
                .headers
                .get(codex_model_provider::ACCOUNT_ROUTING_HEADER)
                .and_then(|value| value.to_str().ok()),
            headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            setup.auth_owner_generation,
            setup.auth_revision,
        ),
        (
            "https://gov.chatgpt.com/backend-api/codex",
            Some("us_cr"),
            Some("account_id"),
            Some(bound.auth_change_state_receiver().borrow().owner_generation),
            Some(*bound.auth_change_receiver().borrow()),
        ),
    );
}

#[tokio::test]
async fn compaction_stream_uses_bearer_after_agent_identity_session_fallback() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let registration_count = Arc::new(AtomicUsize::new(0));
    let response_count = Arc::clone(&registration_count);
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(move |_request: &wiremock::Request| {
            response_count.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(/*status*/ 503)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(core_test_support::responses::sse_response(
            core_test_support::responses::sse(vec![
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {"type": "compaction", "encrypted_content": "summary"},
                }),
                core_test_support::responses::ev_completed("compact-response"),
            ]),
        ))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    let manager = chatgpt_auth_manager(&codex_home, server.uri()).await;
    let mut client = test_openai_model_client(
        manager,
        Some(format!("{}/v1", server.uri())),
        SessionSource::Cli,
    );
    client.agent_identity_policy = AgentIdentityAuthPolicy::ChatGptAuth;
    client.prewarm_auth().await?;
    let mut prompt = test_user_prompt();
    prompt.input.push(ResponseItem::CompactionTrigger {});
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let mut session = client.new_session();
    let mut stream = session
        .stream(
            &prompt,
            &test_model_info(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummaryConfig::None,
            /*service_tier*/ None,
            &metadata,
            &InferenceTraceContext::disabled(),
        )
        .await?;
    while let Some(event) = stream.next().await {
        event?;
    }

    assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    let requests = server.received_requests().await.expect("captured requests");
    let compact_request = requests
        .iter()
        .find(|request| request.url.path() == "/v1/responses")
        .expect("compaction stream request");
    assert_eq!(
        (
            compact_request
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            compact_request
                .headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
        ),
        (Some("Bearer test-access-token"), Some("account-123")),
    );
    let body: serde_json::Value = serde_json::from_slice(&compact_request.body)?;
    assert_eq!(
        body["input"].as_array().unwrap().last(),
        Some(&json!({"type": "compaction_trigger"}))
    );
    Ok(())
}

#[tokio::test]
async fn workspace_routed_http_rejects_redirects_without_a_routing_header() {
    use codex_client::HttpTransport;
    use codex_login::WorkspaceRouting;
    use codex_login::WorkspaceRoutingRequest;
    use codex_login::WorkspaceRoutingResolver;

    struct Routing(Option<&'static str>);
    impl WorkspaceRoutingResolver for Routing {
        fn resolve(
            &self,
            _request: WorkspaceRoutingRequest,
        ) -> Pin<
            Box<
                dyn std::future::Future<Output = std::io::Result<Option<WorkspaceRouting>>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                Ok(self.0.map(|override_value| WorkspaceRouting {
                    chatgpt_account_id: "account_id".into(),
                    backend_origin: "https://gov.chatgpt.com".into(),
                    account_routing_override: override_value.into(),
                }))
            })
        }
    }

    for routing_override in [Some("NO_CONSTRAINT"), Some("us_cr"), None] {
        let origin = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(/*status*/ 307)
                    .insert_header("location", format!("{}/responses", destination.uri())),
            )
            .mount(&origin)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(/*status*/ 200))
            .mount(&destination)
            .await;
        let manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let resolver: Arc<dyn WorkspaceRoutingResolver> = Arc::new(Routing(routing_override));
        manager.set_workspace_routing_resolver(Arc::downgrade(&resolver));
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(manager),
        );
        let mut setup = client
            .current_client_setup(super::ClientRouting::Workspace)
            .await
            .unwrap();
        if routing_override.is_some() {
            assert_eq!(
                setup.api_provider.base_url,
                "https://gov.chatgpt.com/backend-api/codex"
            );
        }
        // Exercise the resolved route's redirect policy against loopback HTTP servers.
        setup.api_provider.base_url = origin.uri();
        let transport = client
            .build_api_transport(&setup.api_provider, "/responses", setup.redirect_policy)
            .unwrap();
        let request = setup
            .api_provider
            .build_request(http::Method::POST, "/responses")
            .with_json(&json!({"input": "workspace content"}));
        let result = transport.execute(request).await;
        if routing_override.is_some() {
            assert!(
                matches!(
                    result,
                    Err(TransportError::Http {
                        retry_after: None,
                        status: http::StatusCode::TEMPORARY_REDIRECT,
                        ..
                    })
                ),
                "workspace redirect must be rejected: {routing_override:?}"
            );
        } else {
            assert_eq!(result.unwrap().status, http::StatusCode::OK);
        }
        assert_eq!(
            destination.received_requests().await.unwrap().len(),
            usize::from(routing_override.is_none())
        );
    }
}

#[derive(Debug)]
enum SetupRefresh {
    Command(PathBuf),
    ChatGpt {
        home: PathBuf,
        token: String,
        workspace: String,
    },
}

#[derive(Debug)]
struct SetupRefreshProvider {
    inner: SharedModelProvider,
    refresh: SetupRefresh,
    setup_calls: AtomicUsize,
}

impl ModelProvider for SetupRefreshProvider {
    fn info(&self) -> &ModelProviderInfo {
        self.inner.info()
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.inner.auth_manager()
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        self.inner.auth()
    }

    fn account_state(&self) -> ProviderAccountResult {
        self.inner.account_state()
    }

    fn api_provider(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<codex_api::Provider>> {
        Box::pin(async move {
            self.setup_calls.fetch_add(1, Ordering::SeqCst);
            let manager = self.inner.auth_manager().expect("auth manager");
            match &self.refresh {
                SetupRefresh::Command(token_path) => {
                    std::fs::write(token_path, "refreshed-token")?;
                    manager
                        .refresh_token_from_authority()
                        .await
                        .expect("refresh command token");
                }
                SetupRefresh::ChatGpt {
                    home,
                    token,
                    workspace,
                } => {
                    codex_login::auth::login_with_chatgpt_auth_tokens(
                        home, token, workspace, /*chatgpt_plan_type*/ None,
                    )?;
                    manager.reload().await;
                }
            }
            self.inner.api_provider().await
        })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.inner.models_manager(codex_home, config_model_catalog)
    }
}

#[tokio::test]
async fn client_setup_accepts_command_credential_refresh() {
    for routing in [
        super::ClientRouting::Workspace,
        super::ClientRouting::ConfiguredProvider,
    ] {
        let tempdir = TempDir::new().unwrap();
        let token_path = tempdir.path().join("token.txt");
        std::fs::write(&token_path, "initial-token").unwrap();
        let mut info = test_model_provider().info().clone();
        info.auth = Some(codex_protocol::config_types::ModelProviderAuthInfo {
            command: if cfg!(windows) { "cmd.exe" } else { "cat" }.into(),
            args: if cfg!(windows) {
                vec!["/D", "/C", "type", "token.txt"]
            } else {
                vec!["token.txt"]
            }
            .into_iter()
            .map(Into::into)
            .collect(),
            timeout_ms: std::num::NonZeroU64::new(/*n*/ 5_000).unwrap(),
            refresh_interval_ms: 60_000,
            cwd: tempdir.path().try_into().unwrap(),
        });
        let provider = Arc::new(SetupRefreshProvider {
            inner: create_model_provider(info, /*auth_manager*/ None),
            refresh: SetupRefresh::Command(token_path),
            setup_calls: AtomicUsize::new(/*v*/ 0),
        });
        let manager = provider.auth_manager().unwrap();
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = provider;

        let setup = client.current_client_setup(routing).await.unwrap();
        let mut headers = http::HeaderMap::new();
        setup.api_auth.add_auth_headers(&mut headers);
        assert_eq!(
            headers.get(http::header::AUTHORIZATION).unwrap(),
            "Bearer refreshed-token"
        );
        let refreshed_revision = Some(*manager.auth_change_receiver().borrow());
        assert_ne!(
            codex_model_provider::ResponsesConnectionKey::new(
                &setup.api_provider,
                setup.auth_revision
            ),
            codex_model_provider::ResponsesConnectionKey::new(
                &setup.api_provider,
                refreshed_revision
            ),
        );
        assert_ne!(setup.auth_owner_generation, client.auth_owner_generation());
    }
}

#[tokio::test]
async fn client_setup_rebuilds_chatgpt_refresh_but_rejects_account_switches() {
    for (user, workspace, expected_calls) in [
        ("user-a", "workspace-a", 2),
        ("user-b", "workspace-a", 1),
        ("user-a", "workspace-b", 1),
    ] {
        let token = |user: &str, revision: &str| {
            let claims =
                json!({"jti": revision, "https://api.openai.com/auth": {"chatgpt_user_id": user}});
            let payload =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
            format!("header.{payload}.signature")
        };
        let home = TempDir::new().unwrap();
        let initial = CodexAuth::from_external_chatgpt_tokens(
            &token("user-a", "initial"),
            "workspace-a",
            /*chatgpt_plan_type*/ None,
        )
        .unwrap();
        let manager =
            AuthManager::from_auth_for_testing_with_home(initial, home.path().to_path_buf());
        let refreshed_token = token(user, "refreshed");
        let mut info = test_model_provider().info().clone();
        info.requires_openai_auth = true;
        let provider = Arc::new(SetupRefreshProvider {
            inner: create_model_provider(info, Some(manager.clone())),
            refresh: SetupRefresh::ChatGpt {
                home: home.path().to_path_buf(),
                token: refreshed_token.clone(),
                workspace: workspace.into(),
            },
            setup_calls: AtomicUsize::new(/*v*/ 0),
        });
        let mut client = test_model_client(SessionSource::Exec);
        Arc::get_mut(&mut client.state).unwrap().provider = provider.clone();
        let result = client
            .current_client_setup(super::ClientRouting::ConfiguredProvider)
            .await;
        if expected_calls == 2 {
            let setup = result.unwrap();
            let mut headers = http::HeaderMap::new();
            setup.api_auth.add_auth_headers(&mut headers);
            assert_eq!(
                headers.get(http::header::AUTHORIZATION).unwrap(),
                &format!("Bearer {refreshed_token}")
            );
            assert_eq!(setup.auth.unwrap().get_token().unwrap(), refreshed_token);
            assert_eq!(
                (setup.auth_revision, setup.auth_owner_generation),
                (Some(*manager.auth_change_receiver().borrow()), Some(0))
            );
        } else {
            assert_eq!(
                result.err().expect("account switch must fail").to_string(),
                "account changed while preparing model request"
            );
        }
        assert_eq!(provider.setup_calls.load(Ordering::SeqCst), expected_calls);
    }
}

fn test_responses_metadata_for_client(
    client: &ModelClient,
    turn_id: Option<&str>,
    window_id: String,
    parent_thread_id: Option<ThreadId>,
    request_kind: TestCodexResponsesRequestKind,
) -> CodexResponsesMetadata {
    let thread_id = client.state.thread_id.to_string();
    test_responses_metadata(
        TEST_INSTALLATION_ID,
        &thread_id,
        &thread_id,
        turn_id,
        window_id,
        &client.state.session_source,
        parent_thread_id,
        request_kind,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "model_messages": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn output_with_tool_result_metadata(metadata: ToolResultMetadata) -> ResponseItem {
    let mut call = ExecutedToolCall::new("test_tool".to_string(), json!({ "query": "keep" }));
    call.set_tool_result_sources(ToolResultSources::new(vec![ToolResultSource {
        r#type: "test_resource".to_string(),
        id: "R1".to_string(),
    }]));
    call.set_tool_result_metadata(metadata);
    let mut output = ResponseItem::from(ResponseInputItem::FunctionCallOutput {
        call_id: "tool-call".to_string(),
        output: FunctionCallOutputPayload::from_text("unchanged tool result".to_string()),
    });
    output.append_executed_tool_calls(vec![call]);
    output.mark_tool_calls_complete();
    output
}

#[tokio::test]
async fn responses_request_includes_internal_metadata_for_provider_grant_or_first_party_destination()
-> anyhow::Result<()> {
    use crate::session::step_context::StepContext;
    use crate::tools::ExecutedToolCalls;
    use crate::tools::context::ToolCallSource;
    use crate::tools::context::ToolPayload;
    use crate::tools::router::ToolCall;
    use codex_features::Feature;
    use codex_features::Features;

    let (_, turn) = crate::session::tests::make_session_and_context().await;
    let step = StepContext::for_test(Arc::new(turn));
    let mut features = Features::default();
    features.enable(Feature::ExecutedToolCallMetadata);
    let recorder = ExecutedToolCalls::new(&features, &codex_history::InitialHistory::New);
    let resource_metadata = json!({"openai/resource_access": {"payload": "x".repeat(600 * 1024)}});
    let mut outputs = Vec::new();
    for id in ["first", "second"] {
        let call = ToolCall {
            tool_name: codex_tools::ToolName::plain("mcp__apps__read"),
            call_id: id.to_string(),
            payload: ToolPayload::Function {
                arguments: json!({"query": id}).to_string(),
            },
            encrypted_function_args: None,
        };
        let (mut recorded, permit) = recorder
            .prepare_direct_call(&call, &ToolCallSource::Direct, &step)
            .expect("direct observation slot");
        recorded.set_tool_result_metadata(ToolResultMetadata::new(&resource_metadata));
        let mut item = ResponseItem::from(ResponseInputItem::FunctionCallOutput {
            call_id: id.to_string(),
            output: FunctionCallOutputPayload::from_text(format!("result for {id}")),
        });
        recorder.attach_direct_call_to_output(&mut item, Some((recorded, permit)));
        outputs.push(item);
    }
    let original_outputs = outputs.clone();
    recorder.attach_to_prompt(&mut outputs, &mut Default::default());
    assert_eq!(outputs, original_outputs);
    let recorded = serde_json::to_value(&outputs)?;
    assert_eq!(
        recorded[0]["internal_chat_message_metadata_passthrough"]["executed_tool_calls"][0]["tool_result_metadata"],
        resource_metadata,
    );
    assert!(
        recorded[1]["internal_chat_message_metadata_passthrough"]["executed_tool_calls"][0]
            ["tool_result_metadata"]
            .as_str()
            .is_some_and(|value| value.starts_with("omitted_due_to_size_limit (overage_bytes="))
    );
    let omitted_output = outputs.pop().expect("second direct output");
    let mut without_omitted_metadata = ResponseItem::from(ResponseInputItem::FunctionCallOutput {
        call_id: "second".to_string(),
        output: FunctionCallOutputPayload::from_text("result for second".to_string()),
    });
    without_omitted_metadata.append_executed_tool_calls(vec![ExecutedToolCall::new(
        "mcp__apps__read".to_string(),
        json!({"query": "second"}),
    )]);
    without_omitted_metadata.mark_tool_calls_complete();

    let mut provider =
        ModelProviderInfo::create_openai_provider(Some("https://api.openai.com/v1".to_string()));
    let mut api_provider = provider.to_api_provider(/*auth_mode*/ None)?;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state)
        .expect("test client should have unique session state")
        .provider = create_model_provider(provider.clone(), /*auth_manager*/ None);
    let output = output_with_tool_result_metadata(ToolResultMetadata::new(&json!({
        "private": { "resource": "raw-result-metadata" },
    })));
    let without_raw_metadata = output_with_tool_result_metadata(ToolResultMetadata::default());
    let attribution = McpAttribution {
        status: McpAttributionStatus::Complete,
        error_reason: None,
        sources: vec![McpAttributionSource {
            connector_id: Some("connector_example".to_string()),
            plugin_id: Some("example@openai-bundled".to_string()),
            server_name: "codex_apps".to_string(),
            tool_name: "search".to_string(),
            first_turn_id: "turn_123".to_string(),
        }],
    };
    let prompt = Prompt {
        input: vec![output.clone(), omitted_output.clone()],
        ..Default::default()
    };
    let mut responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    responses_metadata.mcp_attribution = Some(attribution.clone());
    for (provider_grant, base_url, allowed) in [
        (false, "https://api.openai.com/v1", true),
        (false, "https://chatgpt.com/backend-api/codex", true),
        (false, "https://api.chatgpt-staging.com/v1", true),
        (false, "https://proxy.example.com/v1", false),
        (false, "http://api.openai.com/v1", false),
        (false, "https://api.openai.com.evil.example/v1", false),
        (false, "https://chatgpt.com.evil.example/v1", false),
        (false, "https://api.openai.com@proxy.example.com/v1", false),
        (false, "not a URL", false),
        (true, "http://provider.example/v1", true),
    ] {
        provider.include_internal_metadata = provider_grant;
        Arc::get_mut(&mut client.state)
            .expect("test client should have unique session state")
            .provider = create_model_provider(provider.clone(), /*auth_manager*/ None);
        api_provider.base_url = base_url.to_string();
        let include_internal = client
            .state
            .provider
            .include_internal_metadata(&api_provider);
        for responses_lite in [false, true] {
            let mut model = test_model_info();
            model.use_responses_lite = responses_lite;
            let request = client.build_responses_request(
                &prompt,
                &model,
                /*effort*/ None,
                codex_protocol::config_types::ReasoningSummary::None,
                /*service_tier*/ None,
                &responses_metadata,
                include_internal,
            )?;
            let expected_input = if allowed {
                vec![output.clone(), omitted_output.clone()]
            } else {
                vec![
                    without_raw_metadata.clone(),
                    without_omitted_metadata.clone(),
                ]
            };
            assert!(
                request.input.ends_with(&expected_input),
                "resolved endpoint: {base_url}, responses_lite: {responses_lite}",
            );
            let expected_wire_input = serde_json::to_value(&request.input)?;
            assert_eq!(
                serde_json::to_value(&request)?["input"],
                expected_wire_input
            );
            let expected_json = allowed
                .then(|| serde_json::to_string(&attribution).map(serde_json::Value::String))
                .transpose()?;
            assert_eq!(
                serde_json::to_value(&request)?
                    .get("client_metadata")
                    .and_then(|metadata| metadata.get(MCP_ATTRIBUTION_CLIENT_METADATA_KEY)),
                expected_json.as_ref(),
            );
            let ws_client_metadata = client.build_ws_client_metadata(
                &responses_metadata,
                include_internal,
                responses_lite,
            );
            let ws_request = codex_api::ResponseCreateWsRequest {
                client_metadata: Some(ws_client_metadata),
                ..codex_api::ResponseCreateWsRequest::from(&request)
            };
            let ws_request = serde_json::to_value(ws_request)?;
            assert_eq!(ws_request["input"], expected_wire_input);
            assert_eq!(
                ws_request
                    .get("client_metadata")
                    .and_then(|metadata| metadata.get(MCP_ATTRIBUTION_CLIENT_METADATA_KEY)),
                expected_json.as_ref(),
            );
            assert!(
                serde_json::to_value(&request)?
                    .get("mcp_attribution")
                    .is_none()
            );
            assert_eq!(prompt.input, vec![output.clone(), omitted_output.clone()]);
        }
    }
    responses_metadata.mcp_attribution = Some(McpAttribution {
        status: McpAttributionStatus::AttributionError,
        error_reason: Some(
            codex_protocol::mcp::McpAttributionErrorReason::HistoryMissingCheckpoint,
        ),
        sources: attribution.sources.clone(),
    });
    let request = client.build_responses_request(
        &prompt,
        &test_model_info(),
        /*effort*/ None,
        codex_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &responses_metadata,
        /*include_internal*/ true,
    )?;
    assert_eq!(
        request
            .client_metadata
            .as_ref()
            .and_then(|metadata| metadata.get(MCP_ATTRIBUTION_CLIENT_METADATA_KEY))
            .map(|value| serde_json::from_str::<serde_json::Value>(value))
            .transpose()?,
        Some(json!({
            "status": "attribution_error",
            "sources": attribution.sources,
            "error_reason": "history_missing_checkpoint",
        })),
    );

    let mut oversized_attribution = attribution;
    oversized_attribution.sources[0].tool_name = "a".repeat(MAX_MCP_ATTRIBUTION_BYTES);
    responses_metadata.mcp_attribution = Some(oversized_attribution);
    let request = client.build_responses_request(
        &prompt,
        &test_model_info(),
        /*effort*/ None,
        codex_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &responses_metadata,
        /*include_internal*/ true,
    )?;
    assert_eq!(
        request.client_metadata.as_ref().and_then(|metadata| {
            metadata
                .get(MCP_ATTRIBUTION_CLIENT_METADATA_KEY)
                .map(String::as_str)
        }),
        Some(r#"{"status":"attribution_error","error_reason":"payload_too_large"}"#),
    );
    Ok(())
}

#[test]
fn responses_request_preserves_result_metadata_above_previous_aggregate_budget()
-> anyhow::Result<()> {
    let result_metadata = [
        json!({ "payload": "l".repeat(31 * 1024) }),
        json!({ "payload": "m".repeat(20 * 1024) }),
        json!({ "payload": "m".repeat(20 * 1024) }),
        json!({ "payload": "m".repeat(20 * 1024) }),
        json!({ "payload": "m".repeat(20 * 1024) }),
        json!({ "payload": "m".repeat(20 * 1024) }),
        json!({ "status": "ok" }),
    ];
    let sizes = result_metadata
        .iter()
        .map(|metadata| serde_json::to_vec(metadata).unwrap().len())
        .collect::<Vec<_>>();
    assert!(sizes.iter().all(|bytes| *bytes < 32 * 1024));
    assert!(sizes.iter().sum::<usize>() > 128 * 1024);
    let mut history = Vec::new();
    for (index, metadata) in result_metadata.iter().enumerate() {
        let id = format!("tool-call-{index}");
        history.push(serde_json::from_value(json!({
            "type": "function_call", "call_id": id, "name": "test_tool",
            "arguments": json!({ "query": "keep" }).to_string(),
        }))?);
        let mut output = output_with_tool_result_metadata(ToolResultMetadata::new(metadata));
        let ResponseItem::FunctionCallOutput { call_id, .. } = &mut output else {
            unreachable!("helper returns a function call output");
        };
        *call_id = Some(id);
        history.push(output);
    }
    let original_history = serde_json::to_value(&history)?;

    let mut features = codex_features::Features::default();
    features.enable(codex_features::Feature::ExecutedToolCallMetadata);
    let recorder =
        crate::tools::ExecutedToolCalls::new(&features, &codex_history::InitialHistory::New);
    let mut prompt = Prompt {
        input: history.clone(),
        ..Default::default()
    };
    // Follow the sampling path: budget the request copy before client serialization.
    recorder.attach_to_prompt(&mut prompt.input, &mut Default::default());
    let provider =
        ModelProviderInfo::create_openai_provider(Some("https://api.openai.com/v1".to_string()));
    let api_provider = provider.to_api_provider(/*auth_mode*/ None)?;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state)
        .expect("test client should have unique session state")
        .provider = create_model_provider(provider, /*auth_manager*/ None);
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let request = client.build_responses_request(
        &prompt,
        &test_model_info(),
        /*effort*/ None,
        codex_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &responses_metadata,
        client
            .state
            .provider
            .include_internal_metadata(&api_provider),
    )?;
    let body = serde_json::to_value(&request)?;
    // Whole-input equality covers bindings, arguments, results, sources and completion too.
    assert_eq!(body["input"], original_history);
    let mut without_metadata = body.clone();
    for item in without_metadata["input"].as_array_mut().unwrap() {
        item.as_object_mut()
            .unwrap()
            .remove("internal_chat_message_metadata_passthrough");
    }
    let metadata_bytes =
        serde_json::to_vec(&body)?.len() - serde_json::to_vec(&without_metadata)?.len();
    assert!(metadata_bytes > 128 * 1024);
    assert!(metadata_bytes <= 2 * 1024 * 1024);
    assert_eq!(serde_json::to_value(&history)?, original_history);
    Ok(())
}

#[test]
fn websocket_incremental_reuse_tracks_raw_result_metadata() -> anyhow::Result<()> {
    let provider = ModelProviderInfo {
        include_internal_metadata: false,
        ..ModelProviderInfo::create_openai_provider(Some("https://api.openai.com/v1".to_string()))
    };
    let mut api_provider = provider.to_api_provider(/*auth_mode*/ None)?;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state)
        .expect("test client should have unique session state")
        .provider = create_model_provider(provider, /*auth_manager*/ None);
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    for (scenario, base_url, previous_metadata, current_metadata, expect_incremental) in [
        (
            "late_result",
            "https://api.openai.com/v1",
            None,
            Some("first"),
            false,
        ),
        (
            "unchanged_result",
            "https://api.openai.com/v1",
            Some("first"),
            Some("first"),
            true,
        ),
        (
            "changed_result",
            "https://api.openai.com/v1",
            Some("first"),
            Some("second"),
            false,
        ),
        (
            "ordinary_metadata_only",
            "https://api.openai.com/v1",
            None,
            None,
            true,
        ),
        (
            "filtered_result",
            "https://proxy.example.com/v1",
            None,
            Some("first"),
            true,
        ),
    ] {
        let [mut previous_output, mut current_output] =
            [previous_metadata, current_metadata].map(|metadata| {
                let mut call =
                    ExecutedToolCall::new("apps_tool".to_string(), json!({ "query": "same" }));
                if let Some(id) = metadata {
                    call.set_tool_result_metadata(ToolResultMetadata::new(&json!({ "id": id })));
                }
                let mut output = ResponseItem::from(ResponseInputItem::CustomToolCallOutput {
                    call_id: "exec-call".to_string(),
                    name: None,
                    output: FunctionCallOutputPayload::from_text(
                        "Script running with cell ID cell".to_string(),
                    ),
                });
                output.append_executed_tool_calls(vec![call]);
                output.set_tool_call_cell_id("exec-call");
                output
            });
        previous_output.set_turn_id_if_missing("previous-turn");
        current_output.set_turn_id_if_missing("current-turn");
        api_provider.base_url = base_url.to_string();
        let include_internal = client
            .state
            .provider
            .include_internal_metadata(&api_provider);
        let previous = client.build_responses_request(
            &Prompt {
                input: vec![previous_output],
                ..Default::default()
            },
            &test_model_info(),
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &responses_metadata,
            include_internal,
        )?;
        let follow_up = ResponseItem::from(ResponseInputItem::FunctionCallOutput {
            call_id: "wait-call".to_string(),
            output: FunctionCallOutputPayload::from_text("done".to_string()),
        });
        let current = client.build_responses_request(
            &Prompt {
                input: vec![current_output, follow_up.clone()],
                ..Default::default()
            },
            &test_model_info(),
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &responses_metadata,
            include_internal,
        )?;

        let mut session = client.new_session();
        session.websocket_session.last_request = Some(previous);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(super::LastResponse {
                response_id: "previous-response".to_string(),
                items_added: Vec::new(),
            })
            .unwrap();
        session.websocket_session.last_response_rx = Some(receiver);
        let continuation = session.prepare_websocket_request(&current);
        assert_eq!(
            continuation.map(|continuation| (
                continuation.response_id,
                continuation.items,
                continuation.from_untraced_warmup,
            )),
            expect_incremental.then_some(("previous-response".to_string(), vec![follow_up], false)),
            "{scenario}",
        );
    }
    Ok(())
}

#[tokio::test]
async fn responses_http_preserves_raw_tool_metadata_for_openai_custom_endpoint()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
                )),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let mut provider =
        ModelProviderInfo::create_openai_provider(Some(format!("{}/v1", server.uri())));
    provider.requires_openai_auth = false;
    provider.supports_websockets = false;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state)
        .expect("test client should have unique session state")
        .provider = create_model_provider(provider, /*auth_manager*/ None);
    let output = output_with_tool_result_metadata(ToolResultMetadata::new(&json!({
        "private": "raw-result-metadata",
    })));
    let prompt = Prompt {
        input: vec![output.clone()],
        ..Default::default()
    };
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let mut session = client.new_session();
    let mut stream = session
        .stream(
            &prompt,
            &test_model_info(),
            &test_session_telemetry(),
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &responses_metadata,
            &InferenceTraceContext::disabled(),
        )
        .await?;
    let mut completed = false;
    while let Some(event) = stream.next().await {
        if let ResponseEvent::Completed { response_id, .. } = event? {
            assert_eq!(response_id, "resp-1");
            completed = true;
        }
    }
    assert!(completed);
    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(body["input"], serde_json::to_value(vec![output.clone()])?);
    assert_eq!(prompt.input, vec![output]);
    Ok(())
}

#[test]
fn responses_lite_prefix_ids_track_thread_and_payload() -> anyhow::Result<()> {
    let thread_id = ThreadId::new();
    let client = test_model_client_with_thread_id(thread_id, SessionSource::Cli);
    let mut model = test_model_info();
    model.use_responses_lite = true;
    let mut prompt = Prompt {
        base_instructions: BaseInstructions {
            text: "base instructions".to_string(),
            provenance: None,
        },
        ..Default::default()
    };
    let build = |client: &ModelClient, prompt: &Prompt| {
        client.build_responses_request(
            prompt,
            &model,
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &test_responses_metadata_for_client(
                client,
                /*turn_id*/ None,
                format!("{}:0", client.state.thread_id),
                /*parent_thread_id*/ None,
                TestCodexResponsesRequestKind::Turn,
            ),
            /*include_internal*/ true,
        )
    };

    let original = build(&client, &prompt)?;
    assert_eq!(build(&client, &prompt)?, original);

    prompt.base_instructions.text.push_str(" with an update");
    let changed_instructions = build(&client, &prompt)?;
    assert_eq!(changed_instructions.input[0], original.input[0]);
    assert_ne!(changed_instructions.input[1].id(), original.input[1].id());

    prompt.tools = vec![codex_tools::ToolSpec::Freeform(codex_tools::FreeformTool {
        name: "exec".to_string(),
        description: "Execute JavaScript.".to_string(),
        defer_loading: None,
        format: codex_tools::FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: "start: /.+/".to_string(),
        },
    })]
    .into();
    let changed_tools = build(&client, &prompt)?;
    assert_ne!(
        changed_tools.input[0].id(),
        changed_instructions.input[0].id()
    );
    assert_eq!(changed_tools.input[1], changed_instructions.input[1]);

    let independent = build(
        &test_model_client_with_thread_id(ThreadId::new(), SessionSource::Cli),
        &prompt,
    )?;
    assert_ne!(independent.input[0].id(), changed_tools.input[0].id());
    assert_ne!(independent.input[1].id(), changed_tools.input[1].id());
    Ok(())
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

#[test]
fn websocket_continuation_reset_reason_survives_failed_reconnect_and_turn_boundary() {
    for (reason, later_reason) in [
        ("connection_closed", "other"),
        ("other", "connection_closed"),
    ] {
        let client = test_model_client(SessionSource::Cli);
        let request = client
            .build_responses_request(
                &Prompt::default(),
                &test_model_info(),
                /*effort*/ None,
                codex_protocol::config_types::ReasoningSummary::None,
                /*service_tier*/ None,
                &test_responses_metadata_for_client(
                    &client,
                    /*turn_id*/ None,
                    format!("{}:0", client.state.thread_id),
                    /*parent_thread_id*/ None,
                    TestCodexResponsesRequestKind::Turn,
                ),
                /*include_internal*/ true,
            )
            .expect("build continuation request");
        let mut session = client.new_session();
        session.websocket_session.last_request = Some(request);
        session.websocket_session.reset(Some(reason));
        session.websocket_session.reset(/*reason*/ None);
        session.websocket_session.reset(Some(later_reason));
        drop(session);
        let session = client.new_session();
        assert_eq!(
            session.websocket_session.continuation_reset_reason,
            Some(reason)
        );
    }
}

fn spawned_session_source() -> SessionSource {
    SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 1,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    })
}

fn reasoning_effort_in_request(
    model_info: &ModelInfo,
    session_source: SessionSource,
    effort: ReasoningEffort,
) -> ReasoningEffort {
    let client = test_model_client(session_source);
    client
        .build_responses_request(
            &Prompt::default(),
            model_info,
            Some(effort),
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &test_responses_metadata_for_client(
                &client,
                /*turn_id*/ None,
                format!("{}:0", client.state.thread_id),
                /*parent_thread_id*/ None,
                TestCodexResponsesRequestKind::Turn,
            ),
            /*include_internal*/ true,
        )
        .expect("build responses request")
        .reasoning
        .expect("request should include reasoning")
        .effort
        .expect("request should include reasoning effort")
}

#[test]
fn reasoning_effort_for_requests_uses_multi_agent_override_for_ultra() {
    let mut model_info = test_model_info();
    model_info.multi_agent_reasoning_effort = Some(ReasoningEffort::High);
    model_info
        .supported_reasoning_levels
        .push(ReasoningEffortPreset {
            effort: ReasoningEffort::High,
            description: "high".to_string(),
        });

    let actual = [SessionSource::Cli, spawned_session_source()].map(|session_source| {
        reasoning_effort_in_request(&model_info, session_source, ReasoningEffort::Ultra)
    });

    assert_eq!(actual, [ReasoningEffort::High, ReasoningEffort::High]);
}

#[test]
fn reasoning_effort_for_requests_falls_back_for_missing_or_invalid_override() {
    let mut model_info = test_model_info();
    model_info.supported_reasoning_levels = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffort::Low,
            description: "low".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffort::XHigh,
            description: "xhigh".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffort::Ultra,
            description: "ultra".to_string(),
        },
    ];

    let actual = [
        None,
        Some(ReasoningEffort::Ultra),
        Some(ReasoningEffort::High),
    ]
    .map(|multi_agent_reasoning_effort| {
        model_info.multi_agent_reasoning_effort = multi_agent_reasoning_effort;
        reasoning_effort_in_request(&model_info, SessionSource::Cli, ReasoningEffort::Ultra)
    });

    assert_eq!(
        actual,
        [
            ReasoningEffort::XHigh,
            ReasoningEffort::XHigh,
            ReasoningEffort::XHigh,
        ]
    );

    model_info.multi_agent_reasoning_effort = None;
    model_info.supported_reasoning_levels.insert(
        1,
        ReasoningEffortPreset {
            effort: ReasoningEffort::Max,
            description: "max".to_string(),
        },
    );
    assert_eq!(
        reasoning_effort_in_request(&model_info, SessionSource::Cli, ReasoningEffort::Ultra),
        ReasoningEffort::Max
    );

    model_info.supported_reasoning_levels.clear();
    assert_eq!(
        reasoning_effort_in_request(&model_info, SessionSource::Cli, ReasoningEffort::Ultra),
        ReasoningEffort::Medium
    );
}

#[test]
fn reasoning_effort_for_requests_preserves_non_ultra_and_persistent_behavior() {
    let mut model_info = test_model_info();
    model_info.multi_agent_reasoning_effort = Some(ReasoningEffort::Low);

    assert_eq!(
        (
            reasoning_effort_in_request(&model_info, SessionSource::Cli, ReasoningEffort::High,),
            reasoning_effort_in_request(
                &model_info,
                SessionSource::Cli,
                ReasoningEffort::Persistent,
            ),
        ),
        (
            ReasoningEffort::High,
            ReasoningEffort::Custom("disabled".to_string()),
        )
    );
}

fn write_chatgpt_auth_json(codex_home: &std::path::Path) {
    let auth_json = json!({
        "tokens": {
            "id_token": TEST_CHATGPT_ID_TOKEN,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token",
            "account_id": "account-123"
        },
        "last_refresh": "2099-01-01T00:00:00Z"
    });
    std::fs::write(
        codex_home.join("auth.json"),
        serde_json::to_string_pretty(&auth_json).expect("serialize auth.json"),
    )
    .expect("write auth.json");
}

async fn chatgpt_auth_manager(
    codex_home: &TempDir,
    agent_identity_authapi_base_url: String,
) -> Arc<AuthManager> {
    write_chatgpt_auth_json(codex_home.path());
    let auth_manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await;
    let auth = auth_manager.auth().await.expect("auth should load");
    AuthManager::from_auth_for_testing_with_agent_identity_authapi_base_url(
        auth,
        codex_home.path().to_path_buf(),
        agent_identity_authapi_base_url,
    )
}

#[derive(Default)]
struct TagCollectorVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for TagCollectorVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct TagCollectorLayer {
    tags: Arc<Mutex<BTreeMap<String, String>>>,
}

impl<S> Layer<S> for TagCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "feedback_tags" {
            return;
        }
        let mut visitor = TagCollectorVisitor::default();
        event.record(&mut visitor);
        self.tags.lock().unwrap().extend(visitor.tags);
    }
}

fn started_inference_attempt(temp: &TempDir) -> anyhow::Result<InferenceTraceAttempt> {
    let writer = Arc::new(TraceWriter::create(
        temp.path(),
        "trace-1".to_string(),
        "rollout-1".to_string(),
        "thread-root".to_string(),
    )?);
    writer.append(RawTraceEventPayload::ThreadStarted {
        thread_id: "thread-root".to_string(),
        agent_path: "/root".to_string(),
        metadata_payload: None,
    })?;
    writer.append(RawTraceEventPayload::CodexTurnStarted {
        codex_turn_id: "turn-1".to_string(),
        thread_id: "thread-root".to_string(),
    })?;

    let inference_trace = InferenceTraceContext::enabled(
        writer,
        "thread-root".to_string(),
        "turn-1".to_string(),
        "gpt-test".to_string(),
        "test-provider".to_string(),
    );
    let attempt = inference_trace.start_attempt();
    attempt.record_started(&json!({
        "model": "gpt-test",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }],
    }));
    Ok(attempt)
}

fn output_message(id: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(codex_protocol::ResponseItemId::with_suffix("msg", id)),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

async fn replay_until_cancelled(temp: &TempDir) -> anyhow::Result<RolloutTrace> {
    let mut rollout = replay_bundle(temp.path())?;
    for _ in 0..50 {
        let inference = rollout
            .inference_calls
            .values()
            .next()
            .expect("inference should be reduced");
        if inference.execution.status == ExecutionStatus::Cancelled {
            return Ok(rollout);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        rollout = replay_bundle(temp.path())?;
    }
    Ok(rollout)
}

struct NotifyAfterEventStream {
    events: VecDeque<ResponseEvent>,
    yielded: usize,
    notify_after: usize,
    notify: Arc<Notify>,
}

impl futures::Stream for NotifyAfterEventStream {
    type Item = std::result::Result<ResponseEvent, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(event) = self.events.pop_front() else {
            return Poll::Pending;
        };
        self.yielded += 1;
        if self.yielded == self.notify_after {
            self.notify.notify_one();
        }
        Poll::Ready(Some(Ok(event)))
    }
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[test]
fn internal_session_prompt_cache_key_is_scoped_to_parent_thread() {
    let parent_thread_id = ThreadId::new();
    let client = test_model_client(SessionSource::Internal(InternalSessionSource::Guardian));
    let metadata = test_responses_metadata_for_client(
        &client,
        Some("turn-123"),
        "window-1".to_string(),
        Some(parent_thread_id),
        TestCodexResponsesRequestKind::Turn,
    );

    assert_eq!(
        client.prompt_cache_key(&metadata),
        format!("guardian:{parent_thread_id}")
    );
}

#[test]
fn build_subagent_headers_sets_internal_memory_consolidation_label() {
    let client = test_model_client(SessionSource::Internal(
        InternalSessionSource::MemoryConsolidation,
    ));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
    assert_eq!(
        headers.get("originator"),
        Some(&http::HeaderValue::from_static("test_originator"))
    );
}

#[test]
fn build_ws_client_metadata_includes_window_lineage_and_turn_metadata() {
    let parent_thread_id = ThreadId::new();
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth: 2,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    }));

    let thread_id = client.state.thread_id.to_string();
    let expected_window_id = format!("{thread_id}:1");
    let responses_metadata = test_responses_metadata_for_client(
        &client,
        Some("turn-123"),
        expected_window_id.clone(),
        Some(parent_thread_id),
        TestCodexResponsesRequestKind::Turn,
    );
    let client_metadata = client.build_ws_client_metadata(
        &responses_metadata,
        /*include_internal*/ true,
        /*use_responses_lite*/ false,
    );
    let parent_thread_id = parent_thread_id.to_string();
    let turn_metadata: serde_json::Value = serde_json::from_str(
        client_metadata
            .get(X_CODEX_TURN_METADATA_HEADER)
            .expect("turn metadata"),
    )
    .expect("valid turn metadata");
    for (client_key, metadata_key, expected) in [
        (
            X_CODEX_INSTALLATION_ID_HEADER,
            "installation_id",
            "11111111-1111-4111-8111-111111111111",
        ),
        ("session_id", "session_id", thread_id.as_str()),
        ("thread_id", "thread_id", thread_id.as_str()),
        ("turn_id", "turn_id", "turn-123"),
        (
            X_CODEX_WINDOW_ID_HEADER,
            "window_id",
            expected_window_id.as_str(),
        ),
        (
            X_CODEX_PARENT_THREAD_ID_HEADER,
            "parent_thread_id",
            parent_thread_id.as_str(),
        ),
    ] {
        assert_eq!(
            client_metadata.get(client_key).map(String::as_str),
            Some(expected)
        );
        assert_eq!(turn_metadata[metadata_key].as_str(), Some(expected));
    }
    assert_eq!(
        client_metadata
            .get(X_OPENAI_SUBAGENT_HEADER)
            .map(String::as_str),
        Some("collab_spawn")
    );
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(
            Vec::new(),
            &model_info,
            /*effort*/ None,
            &session_telemetry,
        )
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[tokio::test]
async fn dropped_response_stream_traces_cancelled_partial_output() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;

    // The provider has produced one complete output item, but no terminal
    // response.completed event. The harness has enough information to keep this
    // item in history, so the trace should preserve it when the stream is
    // abandoned.
    let item = output_message("1", "partial answer");
    let api_stream = futures::stream::iter([Ok(ResponseEvent::OutputItemDone(item))])
        .chain(futures::stream::pending());
    let (mut stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
        test_model_provider(),
    );

    let observed = stream
        .next()
        .await
        .expect("mapped stream should yield output item")?;
    assert!(matches!(observed, ResponseEvent::OutputItemDone(_)));

    // Dropping the consumer is how turn interruption/preemption stops polling
    // the provider stream. The mapper task observes that drop asynchronously
    // and records cancellation using the output items it has already seen.
    drop(stream);

    // Cancellation is recorded by the mapper task after Drop wakes it, so the
    // replay may need a short wait before the terminal event appears on disk.
    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[tokio::test]
async fn response_stream_records_last_model_feedback_ids() {
    let tags = Arc::new(Mutex::new(BTreeMap::new()));
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    let api_stream = futures::stream::iter([
        Ok(ResponseEvent::Created { response_id: None }),
        Ok(ResponseEvent::Completed {
            response_id: "resp-123".to_string(),
            token_usage: None,
            usage_metadata: None,
            end_turn: Some(true),
        }),
    ]);
    let (mut stream, _) = super::map_response_events(
        Some("req-123".to_string()),
        api_stream,
        test_session_telemetry(),
        InferenceTraceAttempt::disabled(),
        test_model_provider(),
    );

    while stream.next().await.is_some() {}

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("last_model_request_id").map(String::as_str),
        Some("\"req-123\"")
    );
    assert_eq!(
        tags.get("last_model_response_id").map(String::as_str),
        Some("\"resp-123\"")
    );
}

#[tokio::test]
async fn bedrock_unauthorized_error_uses_provider_mapping() {
    let provider = create_model_provider(
        ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
        /*auth_manager*/ None,
    );
    let mut auth_recovery = None;
    let mut provider_auth_recovery_attempted = false;
    let url = "https://bedrock-mantle.us-east-2.api.aws/openai/v1/responses";
    let error = super::handle_unauthorized(
        TransportError::Http {
            retry_after: None,
            status: http::StatusCode::UNAUTHORIZED,
            url: Some(url.to_string()),
            headers: None,
            body: Some(
                "Signature expired: 20260609T133205Z is now earlier than 20260614T062525Z"
                    .to_string(),
            ),
        },
        &mut auth_recovery,
        &mut provider_auth_recovery_attempted,
        &test_session_telemetry(),
        &provider,
        /*event_sender*/ None,
        /*turn_id*/ None,
    )
    .await
    .expect_err("expired Bedrock signature should fail");

    assert_eq!(
        error.to_string(),
        format!(
            "Amazon Bedrock rejected the request because its AWS signature has expired. Refresh your AWS credentials and retry. If `AWS_BEARER_TOKEN_BEDROCK` is set, update or unset it, then restart Codex, url: {url}"
        )
    );
}

#[derive(Debug)]
struct TestRecoveryProvider {
    inner: SharedModelProvider,
    should_fail: bool,
    attempts: Arc<AtomicUsize>,
}

impl ModelProvider for TestRecoveryProvider {
    fn info(&self) -> &ModelProviderInfo {
        self.inner.info()
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        self.inner.auth()
    }

    fn account_state(&self) -> ProviderAccountResult {
        self.inner.account_state()
    }

    fn auth_recovery_messages(&self) -> Option<ProviderAuthRecoveryMessages> {
        Some(ProviderAuthRecoveryMessages {
            started: "Refreshing provider authentication.",
            succeeded: "Provider authentication recovered.",
        })
    }

    fn recover_from_unauthorized(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<ProviderUnauthorizedRecovery>> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            if self.should_fail {
                Err(CodexErr::Io(std::io::Error::other(
                    "provider recovery failed",
                )))
            } else {
                Ok(ProviderUnauthorizedRecovery::Recovered)
            }
        })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.inner.models_manager(codex_home, config_model_catalog)
    }
}

#[tokio::test]
async fn provider_owned_auth_recovery_is_bounded_and_preserves_unauthorized_failures() {
    for should_fail in [false, true] {
        let attempts = Arc::new(AtomicUsize::new(0));
        let provider: SharedModelProvider = Arc::new(TestRecoveryProvider {
            inner: test_model_provider(),
            should_fail,
            attempts: Arc::clone(&attempts),
        });
        assert!(provider.auth_manager().is_none());

        let unauthorized = || TransportError::Http {
            retry_after: None,
            status: http::StatusCode::UNAUTHORIZED,
            url: Some("https://example.com/v1/responses".to_string()),
            headers: None,
            body: Some("unauthorized".to_string()),
        };
        let mut auth_recovery = None;
        let mut provider_auth_recovery_attempted = false;
        let telemetry = test_session_telemetry();
        let (event_sender, event_receiver) = async_channel::unbounded();
        let result = super::handle_unauthorized(
            unauthorized(),
            &mut auth_recovery,
            &mut provider_auth_recovery_attempted,
            &telemetry,
            &provider,
            Some(&event_sender),
            Some("turn-1"),
        )
        .await;

        let error = if should_fail {
            result.expect_err("failed provider recovery should return the original error")
        } else {
            let recovered = result.expect("provider recovery should succeed without AuthManager");
            assert_eq!(
                (recovered.mode, recovered.phase),
                ("provider", "provider_refresh")
            );
            super::handle_unauthorized(
                unauthorized(),
                &mut auth_recovery,
                &mut provider_auth_recovery_attempted,
                &telemetry,
                &provider,
                Some(&event_sender),
                Some("turn-1"),
            )
            .await
            .expect_err("provider recovery should not run more than once")
        };

        match error.details() {
            CodexErrorDetails::UnexpectedStatus(response) => {
                assert_eq!(response.status, http::StatusCode::UNAUTHORIZED);
                assert_eq!(response.body, "unauthorized");
            }
            other => panic!("unexpected error after provider recovery: {other}"),
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        let events = std::iter::from_fn(|| event_receiver.try_recv().ok())
            .map(|event| serde_json::to_value(event).expect("recovery event should serialize"))
            .collect::<Vec<_>>();
        let mut expected = vec![json!({
            "id": "turn-1",
            "msg": {
                "type": "auth_recovery_started",
                "provider": provider.info().name,
                "message": "Refreshing provider authentication.",
            }
        })];
        if !should_fail {
            expected.push(json!({
                "id": "turn-1",
                "msg": {
                    "type": "auth_recovery_completed",
                    "provider": provider.info().name,
                    "message": "Provider authentication recovered.",
                }
            }));
        }
        assert_eq!(events, expected);
    }
}

#[tokio::test]
async fn dropped_backpressured_response_stream_traces_cancelled_partial_output()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;
    let backpressured_item_yielded = Arc::new(Notify::new());
    let mut events = VecDeque::new();
    for _ in 0..super::RESPONSE_STREAM_CHANNEL_CAPACITY {
        events.push_back(ResponseEvent::Created { response_id: None });
    }
    events.push_back(ResponseEvent::OutputItemDone(output_message(
        "1",
        "partial answer",
    )));
    let api_stream = NotifyAfterEventStream {
        events,
        yielded: 0,
        notify_after: super::RESPONSE_STREAM_CHANNEL_CAPACITY + 1,
        notify: Arc::clone(&backpressured_item_yielded),
    };

    let (stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
        test_model_provider(),
    );

    // Fill the mapper channel with non-terminal events, then yield one output
    // item. The mapper has observed that item and is blocked trying to send it
    // downstream, so dropping the consumer covers the send-failure path rather
    // than the `consumer_dropped` select branch.
    backpressured_item_yielded.notified().await;
    drop(stream);

    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(AuthMode::Chatgpt),
        &BearerAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        /*agent_identity_telemetry*/ None,
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

#[test]
fn auth_request_telemetry_context_tracks_agent_identity_ids() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(AuthMode::Chatgpt),
        &BearerAuthProvider::for_test(/*token*/ None, /*account_id*/ None),
        Some(AgentIdentityTelemetry {
            agent_id: "agent-runtime-context".to_string(),
            task_id: "task-run-context".to_string(),
        }),
        PendingUnauthorizedRetry::default(),
    );

    assert_eq!(
        auth_context.agent_identity_telemetry(),
        Some(&AgentIdentityTelemetry {
            agent_id: "agent-runtime-context".to_string(),
            task_id: "task-run-context".to_string(),
        })
    );
}

fn model_client_with_counting_attestation(
    include_attestation: bool,
) -> (ModelClient, Arc<AtomicUsize>) {
    #[derive(Debug)]
    struct CountingAttestationProvider {
        calls: Arc<AtomicUsize>,
    }

    impl AttestationProvider for CountingAttestationProvider {
        fn header_for_request(
            &self,
            _context: AttestationContext,
        ) -> GenerateAttestationFuture<'_> {
            let calls = self.calls.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::Relaxed) + 1;
                Some(http::HeaderValue::from_bytes(format!("v1.header-{call}").as_bytes()).unwrap())
            })
        }
    }

    let attestation_calls = Arc::new(AtomicUsize::new(0));
    let (auth_manager, provider) = if include_attestation {
        (
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
            ModelProviderInfo::create_openai_provider(Some(CHATGPT_CODEX_BASE_URL.to_string())),
        )
    } else {
        (
            None,
            create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses),
        )
    };
    let model_client = ModelClient::new(
        auth_manager,
        AgentIdentityAuthPolicy::JwtOnly,
        ThreadId::new(),
        provider,
        SessionSource::Exec,
        "test_originator".to_string(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*reasoning_effort_override_enabled*/ false,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        Some(Arc::new(CountingAttestationProvider {
            calls: attestation_calls.clone(),
        })),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        codex_model_provider::WorkspaceRoutingContext::new(
            "https://chatgpt.com/backend-api".into(),
        ),
        Vec::new(),
    );
    (model_client, attestation_calls)
}

#[test]
fn thread_responses_headers_are_scoped_to_model_and_backend_auth() {
    let (mut model_client, _) =
        model_client_with_counting_attestation(/*include_attestation*/ true);
    let headers = http::HeaderMap::from_iter([(
        http::HeaderName::from_static("x-custom-request"),
        http::HeaderValue::from_static("example"),
    )]);
    model_client.codex_responses_headers = Some(Arc::new(crate::CodexResponsesHeaders {
        model: "selected-model".to_owned(),
        headers: headers.clone(),
    }));
    let chatgpt_auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let api_key_auth = CodexAuth::from_api_key("test-api-key");
    for (auth, model, expected) in [
        (Some(&chatgpt_auth), "selected-model", headers),
        (Some(&chatgpt_auth), "other-model", http::HeaderMap::new()),
        (
            Some(&api_key_auth),
            "selected-model",
            http::HeaderMap::new(),
        ),
        (None, "selected-model", http::HeaderMap::new()),
    ] {
        assert_eq!(model_client.responses_headers(auth, model), expected);
    }

    Arc::get_mut(&mut model_client.state)
        .expect("test client should have unique session state")
        .provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(Some("https://proxy.example.com/v1".to_owned())),
        Some(AuthManager::from_auth_for_testing(chatgpt_auth.clone())),
    );
    assert_eq!(
        model_client.responses_headers(Some(&chatgpt_auth), "selected-model"),
        http::HeaderMap::new(),
    );
}

#[test_case::test_case(/*cache_key*/ None; "own_cache")]
#[test_case::test_case(Some("parent-session"); "inherited_cache")]
#[tokio::test]
async fn websocket_handshake_includes_attestation_for_chatgpt_codex_responses(
    cache_key: Option<&str>,
) {
    let (mut model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ true);
    let responses_metadata = test_responses_metadata_for_client(
        &model_client,
        /*turn_id*/ None,
        format!("{}:0", model_client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::WebsocketConnection,
    );

    model_client.prompt_cache_key_override = cache_key.map(str::to_string);
    let headers = model_client
        .build_websocket_headers(&responses_metadata)
        .await;

    assert_eq!(
        headers
            .get(crate::attestation::X_OAI_ATTESTATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("v1.header-1"),
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        headers["session-id"],
        cache_key.unwrap_or(&responses_metadata.session_id)
    );
    assert_eq!(headers["thread-id"], responses_metadata.thread_id);
}

#[tokio::test]
async fn existing_call_sideband_headers_include_attestation() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ true);

    let headers = model_client
        .realtime_sideband_headers(http::HeaderMap::new())
        .await
        .expect("existing call sideband headers should build");

    assert_eq!(
        headers
            .get(crate::attestation::X_OAI_ATTESTATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("v1.header-1"),
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn non_chatgpt_codex_endpoints_omit_attestation_generation() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ false);
    let mut response_headers = http::HeaderMap::new();

    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        response_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut compaction_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        compaction_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut realtime_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        realtime_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }

    assert_eq!(
        response_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        compaction_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        realtime_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn intercepted_output_reaches_trace_and_websocket_bookkeeping() -> anyhow::Result<()> {
    struct ReplaceOutput;
    impl codex_extension_api::ModelResponseInterceptor for ReplaceOutput {
        fn intercept(
            self: Box<Self>,
            stream: codex_extension_api::ModelResponseStream,
        ) -> codex_extension_api::ModelResponseStream {
            Box::pin(stream.map(|event| {
                event.map(|event| match event {
                    ResponseEvent::OutputItemDone(_) => {
                        ResponseEvent::OutputItemDone(output_message("1", "transformed"))
                    }
                    other => other,
                })
            }))
        }
    }

    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;
    let (tx_event, rx_event) = tokio::sync::mpsc::channel(2);
    tx_event
        .send(Ok(ResponseEvent::OutputItemDone(output_message(
            "1", "original",
        ))))
        .await?;
    tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: "response".into(),
            token_usage: None,
            usage_metadata: None,
            end_turn: None,
        }))
        .await?;
    drop(tx_event);
    let (mut stream, last_response) = super::map_response_stream(
        codex_api::ResponseStream {
            rx_event,
            upstream_request_id: None,
            interrupt: None,
        },
        test_session_telemetry(),
        attempt,
        test_model_provider(),
        vec![Box::new(ReplaceOutput)],
    );
    let mut delivered = Vec::new();
    while let Some(event) = stream.next().await {
        if let ResponseEvent::OutputItemDone(item) = event? {
            delivered.push(item);
        }
    }
    assert_eq!(delivered, vec![output_message("1", "transformed")]);
    assert_eq!(last_response.await?.items_added, delivered);
    let rollout = replay_bundle(temp.path())?;
    let payload = rollout
        .raw_payloads
        .values()
        .find(|payload| payload.kind == codex_rollout_trace::RawPayloadKind::InferenceResponse)
        .expect("response trace payload");
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join(&payload.path))?)?;
    assert_eq!(recorded["output_items"], serde_json::to_value(&delivered)?);
    Ok(())
}

struct SwitchingMaintenanceAuth {
    initial: CodexAuth,
    refreshed: CodexAuth,
    did_refresh: std::sync::atomic::AtomicBool,
}

impl ExternalAuth for SwitchingMaintenanceAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move {
            Ok(
                if self.did_refresh.load(std::sync::atomic::Ordering::SeqCst) {
                    self.refreshed.clone()
                } else {
                    self.initial.clone()
                },
            )
        })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move {
            self.did_refresh
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(self.refreshed.clone())
        })
    }
}

#[tokio::test]
async fn captured_maintenance_auth_recovery_cannot_switch_to_another_workspace()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 401)
                .set_body_json(json!({"error":{"message":"expired fixture"}})),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let initial = CodexAuth::from_external_chatgpt_tokens(
        TEST_CHATGPT_ID_TOKEN,
        "initial-workspace",
        Some("pro"),
    )?;
    let refreshed = CodexAuth::from_external_chatgpt_tokens(
        TEST_CHATGPT_ID_TOKEN,
        "different-workspace",
        Some("pro"),
    )?;
    let manager = AuthManager::from_auth_for_testing(initial.clone());
    manager
        .set_external_auth(Arc::new(SwitchingMaintenanceAuth {
            initial: initial.clone(),
            refreshed,
            did_refresh: std::sync::atomic::AtomicBool::new(false),
        }))
        .await?;
    let client = test_openai_model_client(
        manager,
        Some(format!("{}/v1", server.uri())),
        SessionSource::Cli,
    )
    .with_captured_chatgpt_identity(&initial)?;
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        format!("{}:0", client.state.thread_id),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let result = client
        .new_session()
        .stream(
            &test_user_prompt(),
            &test_model_info(),
            &test_session_telemetry(),
            /*effort*/ None,
            ReasoningSummaryConfig::None,
            /*service_tier*/ None,
            &metadata,
            &InferenceTraceContext::disabled(),
        )
        .await;
    let Err(error) = result else {
        panic!("changed workspace must stop auth recovery");
    };
    assert!(
        error.to_string().contains("captured maintenance"),
        "{error}"
    );
    Ok(())
}
