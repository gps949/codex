use super::*;
use crate::AccountAvailability;
use crate::AccountProfile;
use crate::AuthCredentialsStoreMode;
use crate::AuthDotJson;
use crate::AuthKeyringBackendKind;
use crate::TokenData;
use crate::save_auth;
use crate::token_data::parse_chatgpt_jwt_claims;
use chrono::Utc;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;

async fn test_auth_manager(home: &Path) -> Arc<AuthManager> {
    AuthManager::shared(
        home.to_path_buf(),
        false,
        AuthCredentialsStoreMode::File,
        None,
        None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await
}

fn chatgpt_auth(account_id: &str, refresh_token: &str) -> AuthDotJson {
    use base64::Engine;

    #[derive(serde::Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": format!("{account_id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_user_id": account_id,
            "user_id": account_id,
            "chatgpt_account_id": account_id
        }
    });
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = b64(&serde_json::to_vec(&header).expect("header"));
    let payload_b64 = b64(&serde_json::to_vec(&payload).expect("payload"));
    let fake_jwt = format!("{header_b64}.{payload_b64}.sig");
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&fake_jwt).expect("fake jwt"),
            access_token: format!("{account_id}-access"),
            refresh_token: refresh_token.to_string(),
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

fn profile(name: &str, home: PathBuf, priority: u32) -> AccountProfile {
    AccountProfile::new(
        AccountProfileId::new(name).expect("valid id"),
        home,
        priority,
        Some(name.to_string()),
    )
}

#[tokio::test]
async fn disk_relogin_recovers_authentication_unavailable_without_restart() {
    let home = TempDir::new().expect("tempdir");
    let creds = home.path().join("profile-a");
    std::fs::create_dir_all(&creds).expect("creds dir");
    save_auth(
        &creds,
        &chatgpt_auth("acct-a", "old-refresh"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed auth");

    let pool = AccountPool::new();
    let account = profile("profile-a", creds.clone(), 10);
    let manager = test_auth_manager(&creds).await;
    pool.register(account.clone(), Arc::clone(&manager))
        .expect("register");
    let lease = pool.lease().expect("lease");
    pool.mark_authentication_unavailable(&lease, "stale refresh token")
        .expect("mark unavailable");
    assert!(matches!(
        pool.snapshots()[0].availability,
        AccountAvailability::AuthenticationUnavailable { .. }
    ));
    assert!(matches!(
        pool.activate(&account.id),
        Err(AccountPoolError::ProfileUnavailable(_))
    ));

    // Simulate `codex account login <id>` writing fresh tokens while this process is live.
    save_auth(
        &creds,
        &chatgpt_auth("acct-a", "new-refresh"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("relogin auth");

    let recovered = recover_pool_auth_from_disk(&pool).await;
    assert_eq!(recovered, vec![account.id.clone()]);
    assert_eq!(
        pool.snapshots()[0].availability,
        AccountAvailability::Available
    );
    assert_eq!(
        manager
            .auth_cached()
            .expect("auth")
            .get_token_data()
            .expect("token data")
            .refresh_token,
        "new-refresh"
    );
    let activated = pool.activate(&account.id).expect("activate after recover");
    assert_eq!(activated.profile().id, account.id);
}

#[tokio::test]
async fn mismatched_account_id_rewrite_does_not_clear_authentication_unavailable() {
    let home = TempDir::new().expect("tempdir");
    let creds = home.path().join("profile-a");
    std::fs::create_dir_all(&creds).expect("creds dir");
    save_auth(
        &creds,
        &chatgpt_auth("acct-a", "same-refresh"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed auth");

    let pool = AccountPool::new();
    let account = profile("profile-a", creds.clone(), 10);
    let manager = test_auth_manager(&creds).await;
    pool.register(account.clone(), Arc::clone(&manager))
        .expect("register");
    let lease = pool.lease().expect("lease");
    pool.mark_authentication_unavailable(&lease, "permanent refresh failure")
        .expect("mark unavailable");

    // Simulate the 401 permanent-failure path rewriting account_id without rotating refresh.
    save_auth(
        &creds,
        &chatgpt_auth("mismatched-acct", "same-refresh"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("rewrite auth");

    let recovered = recover_pool_auth_from_disk(&pool).await;
    assert!(recovered.is_empty());
    assert!(matches!(
        pool.snapshots()[0].availability,
        AccountAvailability::AuthenticationUnavailable { .. }
    ));
}
