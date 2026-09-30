//! Applies discovered routing and keeps requests within the selected ChatGPT backend.

use codex_login::WorkspaceRouting;
use codex_login::WorkspaceRoutingSession;
use codex_login::default_client::ClientRedirectPolicy;
use http::HeaderValue;
use std::io;
use std::sync::Arc;
use tokio::sync::Mutex;
use url::Url;

pub const ACCOUNT_ROUTING_HEADER: &str = "x-openai-account-routing-override";

/// Routing scope for one session's provider. Once routed, it cannot become
/// independent merely because the account owner's bootstrap configuration changes.
/// Clones serialize discovery and share the first successful routing transition.
#[derive(Clone, Debug)]
pub struct WorkspaceRoutingContext {
    pub(crate) chatgpt_base_url: String,
    pub(crate) previously_routed: Arc<Mutex<bool>>,
    pub(crate) session: Option<Arc<WorkspaceRoutingSession>>,
}

impl WorkspaceRoutingContext {
    pub fn new(chatgpt_base_url: String) -> Self {
        Self {
            chatgpt_base_url,
            previously_routed: Arc::new(Mutex::new(/*t*/ false)),
            session: None,
        }
    }

    pub fn with_session(mut self, session: WorkspaceRoutingSession) -> Self {
        self.session = Some(Arc::new(session));
        self
    }
}

/// Changes only the origin of requests to the selected ChatGPT backend.
pub(crate) fn apply_workspace_routing(
    provider: &mut codex_api::Provider,
    routing: WorkspaceRouting,
) -> io::Result<()> {
    let base_url = &mut provider.base_url;
    let headers = &mut provider.headers;
    let parse = |value: &str| Url::parse(value).map_err(io::Error::other);
    let mut url = parse(base_url)?;
    let backend = parse(&routing.backend_origin)?;
    if backend.scheme() != "https"
        || backend.host_str().is_none()
        || !backend.username().is_empty()
        || backend.password().is_some()
        || backend.path() != "/"
        || backend.query().is_some()
        || backend.fragment().is_some()
    {
        return Err(io::Error::other("invalid workspace backend origin"));
    }
    let header = match routing.account_routing_override.as_str() {
        "NO_CONSTRAINT" => None,
        "us" | "us_cr" => Some(
            HeaderValue::from_str(&routing.account_routing_override).map_err(io::Error::other)?,
        ),
        _ => return Err(io::Error::other("invalid workspace routing override")),
    };
    url.set_scheme(backend.scheme())
        .map_err(|()| io::Error::other("invalid backend scheme"))?;
    url.set_host(backend.host_str()).map_err(io::Error::other)?;
    url.set_port(backend.port())
        .map_err(|()| io::Error::other("invalid backend port"))?;
    *base_url = url.into();
    headers.remove(ACCOUNT_ROUTING_HEADER);
    if let Some(header) = header {
        headers.insert(ACCOUNT_ROUTING_HEADER, header);
    }
    Ok(())
}

/// API deployment and redirect policy resolved for one Responses request.
/// Workspace routes reject redirects even when their routing override supplies no header.
#[derive(Debug)]
pub struct ResolvedResponsesProvider {
    pub provider: codex_api::Provider,
    pub redirect_policy: ClientRedirectPolicy,
}

/// Effective destination and credentials that permit reusing a Responses socket.
#[derive(Debug, PartialEq, Eq)]
pub struct ResponsesConnectionKey {
    base_url: String,
    routing_header: Option<HeaderValue>,
    auth_revision: Option<u64>,
}

impl ResponsesConnectionKey {
    pub fn new(provider: &codex_api::Provider, auth_revision: Option<u64>) -> Self {
        Self {
            base_url: provider.base_url.clone(),
            routing_header: provider.headers.get(ACCOUNT_ROUTING_HEADER).cloned(),
            auth_revision,
        }
    }
}

/// Separates request credentials from the owner of account-scoped discovery.
#[expect(
    clippy::await_holding_invalid_type,
    reason = "serialize discovery and the session's first successful routing transition"
)]
pub(crate) fn resolve_responses_provider<'a, P: crate::ModelProvider + ?Sized>(
    model_provider: &'a P,
    routing_context: &'a WorkspaceRoutingContext,
    routing_owner: &'a codex_login::AuthManager,
) -> crate::ModelProviderFuture<'a, codex_protocol::error::Result<ResolvedResponsesProvider>> {
    Box::pin(async move {
        let mut provider = model_provider.api_provider().await?;
        let mut redirect_policy = ClientRedirectPolicy::Default;
        if crate::provider::provider_uses_first_party_auth_path(model_provider.info())
            && model_provider.info().supports_codex_backend_routes()
            && let Some(auth) = model_provider
                .auth()
                .await
                .filter(codex_login::CodexAuth::is_chatgpt_auth)
        {
            let mut previously_routed = routing_context.previously_routed.lock().await;
            if let Some(routing) = routing_owner
                .workspace_routing(
                    &auth,
                    codex_login::WorkspaceRoutingRequest {
                        provider_base_url: provider.base_url.clone(),
                        chatgpt_base_url: routing_context.chatgpt_base_url.clone(),
                        previously_routed: *previously_routed,
                        session: routing_context.session.clone(),
                    },
                )
                .await?
            {
                crate::workspace_routing::apply_workspace_routing(&mut provider, routing)?;
                redirect_policy = ClientRedirectPolicy::Reject;
                *previously_routed = true;
            }
        }
        Ok(ResolvedResponsesProvider {
            provider,
            redirect_policy,
        })
    })
}
