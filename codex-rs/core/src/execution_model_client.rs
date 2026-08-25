use std::sync::Arc;
use std::sync::Mutex;

use codex_http_client::HttpClientFactory;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::ThreadId;
use codex_protocol::config_types::Verbosity as VerbosityConfig;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::SessionSource;

use crate::attestation::AttestationProvider;
use crate::client::ModelClient;
use crate::client::ModelClientSession;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthLease;

/// Stable, account-independent constructor state for a session-scoped ModelClient.
///
/// Account switching creates another ModelClient rather than mutating the AuthManager under an
/// existing provider/transport. Keeping all other constructor inputs here makes that rebuild
/// deterministic and preserves one logical Codex thread/app-server session.
#[derive(Clone)]
pub(crate) struct ModelClientBlueprint {
    agent_identity_policy: AgentIdentityAuthPolicy,
    thread_id: ThreadId,
    provider_info: ModelProviderInfo,
    session_source: SessionSource,
    originator: String,
    model_verbosity: Option<VerbosityConfig>,
    content_item_kinds_enabled: bool,
    enable_request_compression: bool,
    include_timing_metrics: bool,
    beta_features_header: Option<String>,
    concurrent_reasoning_summaries_enabled: bool,
    attestation_provider: Option<Arc<dyn AttestationProvider>>,
    http_client_factory: HttpClientFactory,
    prompt_cache_key_override: Option<String>,
}

impl ModelClientBlueprint {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        agent_identity_policy: AgentIdentityAuthPolicy,
        thread_id: ThreadId,
        provider_info: ModelProviderInfo,
        session_source: SessionSource,
        originator: String,
        model_verbosity: Option<VerbosityConfig>,
        content_item_kinds_enabled: bool,
        enable_request_compression: bool,
        include_timing_metrics: bool,
        beta_features_header: Option<String>,
        concurrent_reasoning_summaries_enabled: bool,
        attestation_provider: Option<Arc<dyn AttestationProvider>>,
        http_client_factory: HttpClientFactory,
    ) -> Self {
        Self {
            agent_identity_policy,
            thread_id,
            provider_info,
            session_source,
            originator,
            model_verbosity,
            content_item_kinds_enabled,
            enable_request_compression,
            include_timing_metrics,
            beta_features_header,
            concurrent_reasoning_summaries_enabled,
            attestation_provider,
            http_client_factory,
            prompt_cache_key_override: None,
        }
    }

    pub(crate) fn with_prompt_cache_key_override(
        mut self,
        prompt_cache_key_override: Option<String>,
    ) -> Self {
        self.prompt_cache_key_override = prompt_cache_key_override;
        self
    }

    fn build(&self, lease: &ExecutionAuthLease) -> ModelClient {
        ModelClient::new(
            Some(lease.auth_manager()),
            self.agent_identity_policy,
            self.thread_id,
            self.provider_info.clone(),
            self.session_source.clone(),
            self.originator.clone(),
            self.model_verbosity,
            self.content_item_kinds_enabled,
            self.enable_request_compression,
            self.include_timing_metrics,
            self.beta_features_header.clone(),
            self.concurrent_reasoning_summaries_enabled,
            self.attestation_provider.clone(),
            self.http_client_factory.clone(),
        )
        .with_prompt_cache_key_override(self.prompt_cache_key_override.clone())
    }
}

struct AccountBoundModelClient {
    lease: ExecutionAuthLease,
    client: ModelClient,
}

/// Session-level inference-client owner.
///
/// The logical Codex session stays stable while only account-bound model/provider/transport state
/// is replaced after a scheduler transition.
#[derive(Clone)]
pub(crate) struct ExecutionModelClient {
    execution_auth: Arc<ExecutionAuth>,
    blueprint: ModelClientBlueprint,
    cached: Arc<Mutex<Option<AccountBoundModelClient>>>,
}

impl ExecutionModelClient {
    pub(crate) fn new(execution_auth: Arc<ExecutionAuth>, blueprint: ModelClientBlueprint) -> Self {
        Self {
            execution_auth,
            blueprint,
            cached: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn execution_auth(&self) -> &Arc<ExecutionAuth> {
        &self.execution_auth
    }

    /// Pins the currently selected execution identity into a concrete stock ModelClient. Long-lived
    /// protocols such as Realtime must keep this returned client for their full lifetime so a
    /// later pool transition cannot mix call-create and sideband authentication.
    pub(crate) fn bind_active_client(
        &self,
    ) -> std::io::Result<(ExecutionAuthLease, ModelClient)> {
        let lease = self.execution_auth.active_lease().ok_or_else(|| {
            std::io::Error::other("no schedulable Codex execution account is available")
        })?;
        let client = self.client_for_lease(&lease)?;
        Ok((lease, client))
    }

    pub(crate) fn auth_manager(&self) -> Option<Arc<codex_login::AuthManager>> {
        self.execution_auth
            .active_lease()
            .map(|lease| lease.auth_manager())
    }

    pub(crate) fn responses_websocket_enabled(&self) -> bool {
        let Some(lease) = self.execution_auth.active_lease() else {
            return false;
        };
        self.client_for_lease(&lease)
            .map(|client| client.responses_websocket_enabled())
            .unwrap_or(false)
    }

    pub(crate) async fn prewarm_auth(&self) -> CodexResult<()> {
        let lease = self.execution_auth.active_lease().ok_or_else(|| {
            std::io::Error::other("no schedulable Codex execution account is available")
        })?;
        self.client_for_lease(&lease)?.prewarm_auth().await
    }

    pub(crate) fn new_session(&self) -> std::io::Result<ExecutionModelClientSession> {
        let lease = self.execution_auth.active_lease().ok_or_else(|| {
            std::io::Error::other("no schedulable Codex execution account is available")
        })?;
        self.new_session_for_lease(lease)
    }

    pub(crate) fn new_session_for_lease(
        &self,
        lease: ExecutionAuthLease,
    ) -> std::io::Result<ExecutionModelClientSession> {
        let client = self.client_for_lease(&lease)?;
        Ok(ExecutionModelClientSession {
            lease,
            inner: client.new_session(),
        })
    }

    /// Closes the race where account A is prewarmed and another worker switches the pool to B
    /// before the first real sampling request starts.
    pub(crate) fn rebind_session_to_active(
        &self,
        session: &mut ExecutionModelClientSession,
    ) -> std::io::Result<bool> {
        let lease = self.execution_auth.active_lease().ok_or_else(|| {
            std::io::Error::other("no schedulable Codex execution account is available")
        })?;
        if session.lease.is_same_execution_identity(&lease) {
            return Ok(false);
        }
        self.rebind_session(session, lease)?;
        Ok(true)
    }

    /// Rebinds an in-progress logical turn to another execution identity.
    ///
    /// `ModelClient::new_session` intentionally discards the old WebSocket, previous-response
    /// transport state and x-codex-turn-state. No account-scoped transport object crosses the
    /// transition boundary.
    pub(crate) fn rebind_session(
        &self,
        session: &mut ExecutionModelClientSession,
        lease: ExecutionAuthLease,
    ) -> std::io::Result<()> {
        if session.lease.is_same_execution_identity(&lease) {
            return Ok(());
        }
        let client = self.client_for_lease(&lease)?;
        session.inner = client.new_session();
        session.lease = lease;
        Ok(())
    }

    fn client_for_lease(&self, lease: &ExecutionAuthLease) -> std::io::Result<ModelClient> {
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock execution model client"))?;
        if let Some(bound) = cached.as_ref()
            && bound.lease.is_same_execution_identity(lease)
        {
            return Ok(bound.client.clone());
        }

        let client = self.blueprint.build(lease);
        *cached = Some(AccountBoundModelClient {
            lease: lease.clone(),
            client: client.clone(),
        });
        Ok(client)
    }
}

/// Turn-scoped model session paired with the immutable account generation that created it.
pub(crate) struct ExecutionModelClientSession {
    lease: ExecutionAuthLease,
    inner: ModelClientSession,
}

impl ExecutionModelClientSession {
    pub(crate) fn execution_lease(&self) -> &ExecutionAuthLease {
        &self.lease
    }

    pub(crate) fn turn_state(&self) -> Arc<std::sync::OnceLock<String>> {
        self.inner.turn_state()
    }

    pub(crate) async fn preconnect_websocket(
        &mut self,
        session_telemetry: &codex_otel::SessionTelemetry,
        responses_metadata: &crate::responses_metadata::CodexResponsesMetadata,
    ) -> std::result::Result<(), codex_api::ApiError> {
        self.inner
            .preconnect_websocket(session_telemetry, responses_metadata)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prewarm_websocket(
        &mut self,
        prompt: &crate::client_common::Prompt,
        model_info: &codex_protocol::openai_models::ModelInfo,
        session_telemetry: &codex_otel::SessionTelemetry,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
        summary: codex_protocol::config_types::ReasoningSummary,
        service_tier: Option<String>,
        responses_metadata: &crate::responses_metadata::CodexResponsesMetadata,
    ) -> codex_protocol::error::Result<()> {
        self.inner
            .prewarm_websocket(
                prompt,
                model_info,
                session_telemetry,
                effort,
                summary,
                service_tier,
                responses_metadata,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn stream(
        &mut self,
        prompt: &crate::client_common::Prompt,
        model_info: &codex_protocol::openai_models::ModelInfo,
        session_telemetry: &codex_otel::SessionTelemetry,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
        summary: codex_protocol::config_types::ReasoningSummary,
        service_tier: Option<String>,
        responses_metadata: &crate::responses_metadata::CodexResponsesMetadata,
        inference_trace: &codex_rollout_trace::InferenceTraceContext,
    ) -> codex_protocol::error::Result<crate::client_common::ResponseStream> {
        self.inner
            .stream(
                prompt,
                model_info,
                session_telemetry,
                effort,
                summary,
                service_tier,
                responses_metadata,
                inference_trace,
            )
            .await
    }

    pub(crate) fn try_switch_fallback_transport(
        &mut self,
        session_telemetry: &codex_otel::SessionTelemetry,
        model_info: &codex_protocol::openai_models::ModelInfo,
    ) -> bool {
        self.inner
            .try_switch_fallback_transport(session_telemetry, model_info)
    }
}
