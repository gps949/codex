use super::*;
use base64::Engine as _;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

struct IndependentRouting {
    called: AtomicBool,
}

impl WorkspaceRoutingResolver for IndependentRouting {
    fn resolve(
        &self,
        _request: WorkspaceRoutingRequest,
    ) -> Pin<Box<dyn Future<Output = io::Result<Option<WorkspaceRouting>>> + Send + '_>> {
        Box::pin(async {
            self.called.store(true, Ordering::SeqCst);
            Ok(None)
        })
    }
}

fn auth(workspace: &str, token: &str) -> CodexAuth {
    let claims =
        json!({"jti": token, "https://api.openai.com/auth": {"chatgpt_user_id": "same-user"}});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    CodexAuth::from_external_chatgpt_tokens(
        &format!("header.{payload}.signature"),
        workspace,
        Some("business"),
    )
    .expect("valid test auth")
}

#[tokio::test]
async fn bound_request_rejects_another_workspace_even_when_discovery_has_no_route() {
    let manager = AuthManager::from_auth_for_testing(auth("workspace-a", "token-a"));
    let resolver = Arc::new(IndependentRouting {
        called: AtomicBool::new(false),
    });
    let owner: Arc<dyn WorkspaceRoutingResolver> = resolver.clone();
    manager.set_workspace_routing_resolver(Arc::downgrade(&owner));
    let result = manager
        .workspace_routing(
            &auth("workspace-b", "token-b"),
            WorkspaceRoutingRequest {
                provider_base_url: "https://chatgpt.com/backend-api/codex".into(),
                chatgpt_base_url: "https://chatgpt.com/backend-api/".into(),
                previously_routed: false,
                session: None,
            },
        )
        .await;
    assert_eq!(
        result
            .expect_err("different workspace must fail closed")
            .to_string(),
        "request account does not match workspace routing owner"
    );
    assert!(!resolver.called.load(Ordering::SeqCst));
}

#[tokio::test]
async fn bound_request_allows_same_owner_after_credential_refresh() -> io::Result<()> {
    let manager = AuthManager::from_auth_for_testing(auth("workspace-a", "new-token"));
    let resolver = Arc::new(IndependentRouting {
        called: AtomicBool::new(false),
    });
    let owner: Arc<dyn WorkspaceRoutingResolver> = resolver.clone();
    manager.set_workspace_routing_resolver(Arc::downgrade(&owner));
    let result = manager
        .workspace_routing(
            &auth("workspace-a", "previous-token"),
            WorkspaceRoutingRequest {
                provider_base_url: "https://chatgpt.com/backend-api/codex".into(),
                chatgpt_base_url: "https://chatgpt.com/backend-api/".into(),
                previously_routed: false,
                session: None,
            },
        )
        .await?;
    assert_eq!(result, None);
    assert!(resolver.called.load(Ordering::SeqCst));
    Ok(())
}
