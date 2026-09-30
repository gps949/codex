use super::*;
use base64::Engine;
use codex_http_client::DestinationPolicy;
use codex_http_client::NetworkPolicyController;
use codex_http_client::NetworkPolicyDenied;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Mutex;

fn chatgpt_auth(user: &str) -> CodexAuth {
    let claims = json!({
        "jti": user,
        "https://api.openai.com/auth": {"chatgpt_user_id": user},
    });
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    CodexAuth::from_external_chatgpt_tokens(
        &format!("header.{payload}.signature"),
        "workspace-a",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap()
}

struct SwitchableExternalAuth {
    current: Mutex<CodexAuth>,
    rotates_within_application_login: bool,
}

impl ExternalAuth for SwitchableExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move { Ok(self.current.lock().unwrap().clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async move { Ok(self.current.lock().unwrap().clone()) })
    }

    fn rotates_within_application_login(&self) -> bool {
        self.rotates_within_application_login
    }
}

/// Switches the external source to another user and reports whether a request that captured the
/// previous account's client can still start, plus how far the owner generation advanced.
async fn switch_external_owner(
    rotates_within_application_login: bool,
) -> (Result<(), NetworkPolicyDenied>, u64) {
    let mut manager = AuthManager::from_optional_auth_for_testing(/*auth*/ None);
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    Arc::get_mut(&mut manager).unwrap().auth_route_config =
        AuthRouteConfig::from_http_client_factory(
            manager
                .http_client_factory()
                .with_network_policy(policy.clone()),
        );
    let source = Arc::new(SwitchableExternalAuth {
        current: Mutex::new(chatgpt_auth("user-a")),
        rotates_within_application_login,
    });
    manager.set_external_auth(source.clone()).await.unwrap();
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let owner_generation_before = manager
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    let factory = manager.http_client_factory();

    *source.current.lock().unwrap() = chatgpt_auth("user-b");
    manager.reload().await;

    let owner_generation_after = manager
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    let endpoint = "https://example.com/".parse().unwrap();
    (
        factory.network_policy().acquire(&endpoint).map(|_| ()),
        owner_generation_after - owner_generation_before,
    )
}

#[tokio::test]
async fn pool_rotation_keeps_outstanding_requests_but_still_changes_the_owner() {
    assert_eq!(
        switch_external_owner(/*rotates_within_application_login*/ true).await,
        (Ok(()), 1),
    );
}

#[tokio::test]
async fn clearing_a_pool_style_external_source_does_not_deadlock_and_revokes() {
    let mut manager = AuthManager::from_optional_auth_for_testing(/*auth*/ None);
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    Arc::get_mut(&mut manager).unwrap().auth_route_config =
        AuthRouteConfig::from_http_client_factory(
            manager
                .http_client_factory()
                .with_network_policy(policy.clone()),
        );
    let source = Arc::new(SwitchableExternalAuth {
        current: Mutex::new(chatgpt_auth("user-a")),
        rotates_within_application_login: true,
    });
    manager.set_external_auth(source).await.unwrap();
    assert!(controller.publish(policy.revision(), DestinationPolicy::Unrestricted));
    let factory = manager.http_client_factory();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let clearing = Arc::clone(&manager);
    std::thread::spawn(move || {
        clearing.clear_external_auth();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("clear_external_auth must not wait on its own provider lock");

    let endpoint = "https://example.com/".parse().unwrap();
    assert_eq!(
        factory.network_policy().acquire(&endpoint).map(|_| ()),
        Err(NetworkPolicyDenied::Revoked),
    );
}

#[tokio::test]
async fn other_external_owner_changes_revoke_outstanding_requests() {
    assert_eq!(
        switch_external_owner(/*rotates_within_application_login*/ false).await,
        (Err(NetworkPolicyDenied::Revoked), 1),
    );
}
