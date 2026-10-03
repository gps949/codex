use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pretty_assertions::assert_eq;
use tokio::sync::oneshot;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;
use crate::AccountLoginOutcomeKind;
use crate::AccountProfileState;

async fn fixture() -> (
    tempfile::TempDir,
    AccountProfileStore,
    PendingAccountDeviceLogin,
    MockServer,
) {
    let home = tempfile::TempDir::new().unwrap();
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let server = MockServer::start().await;
    let claims = serde_json::json!({"https://api.openai.com/auth": {
        "chatgpt_user_id": "fixture-user", "chatgpt_account_id": "fixture-workspace"
    }});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "fixture-device", "user_code": "ABCD-EFGH", "interval": "1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "authorization_code": "fixture-code", "code_challenge": "fixture-challenge",
            "code_verifier": "fixture-verifier"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": format!("e30.{payload}.sig"), "access_token": "fixture-access",
            "refresh_token": "fixture-refresh", "token_type": "Bearer", "expires_in": 3600
        })))
        .mount(&server)
        .await;
    let mut options = ServerOptions::new(
        home.path().to_path_buf(),
        crate::CLIENT_ID.to_owned(),
        /*forced_chatgpt_workspace_id*/ None,
        AuthCredentialsStoreMode::File,
        crate::auth::AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    );
    options.issuer = server.uri();
    let pending = begin_account_device_login(
        store.clone(),
        options,
        /*label*/ None,
        /*priority*/ 10,
    )
    .await
    .unwrap();
    (home, store, pending, server)
}

async fn wait_until(predicate: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(5), predicate)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_device_login_waits_for_started_persistence_before_removal() {
    let (_home, store, pending, server) = fixture().await;
    let profile = pending.profile().clone();
    let staging_home = pending.options.as_ref().unwrap().codex_home.clone();
    assert_ne!(staging_home, profile.credential_home);
    let persistence_guard = crate::account_file::refresh_lock(&staging_home).unwrap();
    let (cancel, cancelled) = oneshot::channel();
    let mut completion = tokio::spawn(pending.complete_with_cancellation(async move {
        let _ = cancelled.await;
    }));
    wait_until(async {
        loop {
            if server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path() == "/oauth/token")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    // The actual credential save is held at its refresh lock. Cancellation may stop HTTP,
    // but must not drop the already-started blocking save or race its cleanup.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut completion)
            .await
            .is_err()
    );
    cancel.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut completion)
            .await
            .is_err()
    );
    assert_eq!(store.load_profile_records().unwrap().len(), 1);
    drop(persistence_guard);
    assert!(matches!(
        completion.await.unwrap(),
        Err(AccountLoginFlowError::Cancelled)
    ));
    assert_eq!(store.load_profile_records().unwrap(), Vec::new());
    assert!(!profile.credential_home.exists());
    assert!(!staging_home.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_device_login_success_wins_cancellation_during_commit() {
    let (home, store, pending, _server) = fixture().await;
    let profile = pending.profile().clone();
    let commit_guard = crate::account_file::refresh_lock(&profile.credential_home).unwrap();
    let home_path = Arc::new(home.path().to_path_buf());
    let (cancel, cancelled) = oneshot::channel();
    let completion = tokio::spawn(pending.complete_with_cancellation(async move {
        let _ = cancelled.await;
    }));
    wait_until(async {
        loop {
            if crate::account_file::try_lock(&home_path).unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    cancel.send(()).unwrap();
    drop(commit_guard);
    let outcome = completion.await.unwrap().unwrap();
    assert_eq!(
        (outcome.profile.clone(), outcome.kind),
        (profile.clone(), AccountLoginOutcomeKind::Added)
    );
    assert_eq!(
        store.load_profile_records().unwrap(),
        vec![crate::AccountProfileRecord {
            profile: outcome.profile,
            state: AccountProfileState::Ready,
        }]
    );
    assert!(
        crate::auth::load_auth_dot_json(
            &profile.credential_home,
            AuthCredentialsStoreMode::File,
            crate::auth::AuthKeyringBackendKind::default()
        )
        .unwrap()
        .is_some()
    );
}

#[tokio::test]
async fn cancellation_before_device_request_removes_only_the_pending_profile() {
    let home = tempfile::TempDir::new().unwrap();
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let options = ServerOptions::new(
        home.path().to_path_buf(),
        crate::CLIENT_ID.to_owned(),
        /*forced_chatgpt_workspace_id*/ None,
        AuthCredentialsStoreMode::File,
        crate::auth::AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    );
    let pending = prepare_account_device_login(
        store.clone(),
        options,
        /*label*/ None,
        /*priority*/ 10,
    )
    .unwrap();
    let staging_home = pending.options.as_ref().unwrap().codex_home.clone();
    assert!(matches!(
        pending
            .request_code_with_cancellation(std::future::ready(()))
            .await,
        Err(AccountLoginFlowError::Cancelled)
    ));
    assert_eq!(store.load_profile_records().unwrap(), Vec::new());
    assert!(!staging_home.exists());
}
