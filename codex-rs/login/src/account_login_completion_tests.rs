use std::sync::Arc;

use base64::Engine;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::Barrier;

use super::*;
use crate::AccountProfileState;
use crate::auth::AuthDotJson;
use crate::auth::load_auth_dot_json;
use crate::auth::save_auth;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;

fn credentials() -> AuthDotJson {
    let payload = serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "same-user",
            "chatgpt_account_id": "same-workspace"
        }
    });
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&format!("e30.{encoded}.sig")).unwrap(),
            access_token: "fixture-access".into(),
            refresh_token: "fixture-refresh".into(),
            account_id: Some("same-workspace".into()),
        }),
        last_refresh: Some(chrono::Utc::now()),
        openai_api_key: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_seat_login_completion_keeps_one_ready_profile() {
    let home = TempDir::new().unwrap();
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let auth = AuthPersistenceConfig {
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        auth_keyring_backend_kind: crate::auth::AuthKeyringBackendKind::default(),
    };
    let stored_auth = credentials();
    let pending: Vec<_> = (0..4)
        .map(|priority| store.allocate_profile(/*label*/ None, priority).unwrap())
        .collect();
    let barrier = Arc::new(Barrier::new(pending.len()));
    let mut tasks = Vec::new();
    for profile in &pending {
        save_auth(
            &profile.credential_home,
            &stored_auth,
            auth.auth_credentials_store_mode,
            auth.auth_keyring_backend_kind,
        )
        .unwrap();
        let store = store.clone();
        let profile = profile.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            finish_successful_login(
                store,
                profile,
                AccountLoginMode::NewProfile,
                auth,
                /*relogin*/ None,
            )
            .await
            .unwrap()
        }));
    }
    let mut outcomes = Vec::new();
    for task in tasks {
        outcomes.push(task.await.unwrap());
    }
    let canonical = outcomes[0].profile.clone();
    assert_eq!(
        store.load_profile_records().unwrap(),
        vec![crate::AccountProfileRecord {
            profile: canonical.clone(),
            state: AccountProfileState::Ready,
        }]
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.kind == AccountLoginOutcomeKind::Added)
            .count(),
        1
    );
    assert!(outcomes.iter().all(|outcome| outcome.profile == canonical));
    assert_eq!(
        load_auth_dot_json(
            &canonical.credential_home,
            auth.auth_credentials_store_mode,
            auth.auth_keyring_backend_kind,
        )
        .unwrap(),
        Some(stored_auth)
    );
    assert!(
        pending
            .iter()
            .all(|profile| { profile.id == canonical.id || !profile.credential_home.exists() })
    );
}

#[tokio::test]
async fn completing_a_cancelled_login_does_not_recreate_its_credentials() {
    let home = TempDir::new().unwrap();
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store
        .allocate_profile(/*label*/ None, /*priority*/ 10)
        .unwrap();
    let auth = AuthPersistenceConfig {
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        auth_keyring_backend_kind: crate::auth::AuthKeyringBackendKind::default(),
    };
    let staging = ReloginStaging::new(
        store.codex_home(),
        &profile,
        auth.auth_credentials_store_mode,
        auth.auth_keyring_backend_kind,
    )
    .unwrap();
    save_auth(
        staging.home(),
        &credentials(),
        AuthCredentialsStoreMode::File,
        auth.auth_keyring_backend_kind,
    )
    .unwrap();
    store.abandon_pending_profile(&profile.id).unwrap();

    assert!(
        finish_successful_login(
            store.clone(),
            profile.clone(),
            AccountLoginMode::Relogin,
            auth,
            Some(staging),
        )
        .await
        .is_err()
    );
    assert!(!profile.credential_home.exists());
    assert_eq!(store.load_profile_records().unwrap(), Vec::new());
}
