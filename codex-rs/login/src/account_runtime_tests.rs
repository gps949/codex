use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Duration;
use chrono::Utc;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::AccountPoolRuntime;
use super::restore_runtime_state;
use crate::AccountAvailability;
use crate::AccountPool;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileStore;
use crate::AccountRuntimeProfileState;
use crate::AccountRuntimeState;
use crate::AuthConfig;
use crate::AuthDotJson;
use crate::AuthKeyringBackendKind;
use crate::AuthManager;
use crate::TokenData;
use crate::save_auth;

async fn test_auth_manager(home: &Path) -> std::sync::Arc<AuthManager> {
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

fn profile(name: &str, priority: u32) -> AccountProfile {
    AccountProfile::new(
        AccountProfileId::new(name).expect("valid profile id"),
        PathBuf::from(format!("/tmp/{name}")),
        priority,
        None,
    )
}

fn chatgpt_auth(account_id: &str) -> AuthDotJson {
    use crate::token_data::parse_chatgpt_jwt_claims;
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
    let header_b64 = b64(&serde_json::to_vec(&header).expect("header should serialize"));
    let payload_b64 = b64(&serde_json::to_vec(&payload).expect("payload should serialize"));
    let fake_jwt = format!("{header_b64}.{payload_b64}.sig");

    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&fake_jwt).expect("fake jwt should parse"),
            access_token: format!("{account_id}-access"),
            refresh_token: format!("{account_id}-refresh"),
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

fn test_auth_config(codex_home: PathBuf) -> AuthConfig {
    AuthConfig {
        codex_home,
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_login_method: None,
        chatgpt_base_url: None,
        forced_chatgpt_workspace_id: None,
        managed_auth_policy: ManagedAuthPolicy::default(),
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    }
}

/// A persisted state where every profile is still cooling down must restore into a valid pool
/// (reporting the cooldowns) instead of failing the whole runtime install.
#[tokio::test]
async fn restoring_a_fully_cooling_down_pool_succeeds() {
    let pool = AccountPool::new();
    let first = profile("first", 10);
    let second = profile("second", 20);
    for account in [&first, &second] {
        pool.register(
            account.clone(),
            test_auth_manager(&account.credential_home).await,
        )
        .expect("register account");
    }

    let resets_at = Utc::now() + Duration::hours(2);
    let state = AccountRuntimeState {
        active_profile_id: Some(first.id.clone()),
        profiles: vec![
            AccountRuntimeProfileState {
                profile_id: first.id.clone(),
                exhausted_until: Some(resets_at),
                rate_limits: Default::default(),
            },
            AccountRuntimeProfileState {
                profile_id: second.id.clone(),
                exhausted_until: Some(resets_at),
                rate_limits: Default::default(),
            },
        ],
    };

    restore_runtime_state(&pool, &state).expect("cooldown-only restore must succeed");

    let snapshots = pool.snapshots();
    assert_eq!(snapshots.len(), 2);
    for snapshot in snapshots {
        assert_eq!(
            snapshot.availability,
            AccountAvailability::Exhausted {
                resets_at: Some(resets_at)
            }
        );
    }
}

/// Remote control keeps a separate root AuthManager. Installing the pool onto the execution
/// manager and rotating away from legacy-root must not change the root-pinned identity.
#[tokio::test]
async fn root_auth_manager_stays_pinned_when_pool_rotates_execution_identity() {
    let codex_home = TempDir::new().expect("temp dir");
    save_auth(
        codex_home.path(),
        &chatgpt_auth("root-account"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save root auth");

    let store = AccountProfileStore::new(codex_home.path().to_path_buf());
    let secondary = store
        .allocate_profile(Some("secondary".to_string()), 10)
        .expect("allocate secondary");
    save_auth(
        &secondary.credential_home,
        &chatgpt_auth("secondary-account"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save secondary auth");
    store
        .complete_profile(&secondary.id)
        .expect("complete secondary");

    let execution_auth_manager = Arc::clone(&test_auth_manager(codex_home.path()).await);
    let remote_control_auth_manager = test_auth_manager(codex_home.path()).await;

    let runtime = AccountPoolRuntime::install(
        Arc::clone(&execution_auth_manager),
        test_auth_config(codex_home.path().to_path_buf()),
        /*include_existing_root_login*/ true,
    )
    .await
    .expect("install pool");

    runtime
        .pool()
        .activate(&secondary.id)
        .expect("activate secondary");
    // activate bumps the pool generation; the runtime sync task may already have reloaded the
    // execution manager. Call reload anyway so the assertion does not race that background task.
    let _ = execution_auth_manager.reload().await;

    let execution_account = execution_auth_manager
        .auth()
        .await
        .expect("execution auth")
        .get_account_id();
    let remote_control_account = remote_control_auth_manager
        .auth()
        .await
        .expect("remote-control auth")
        .get_account_id();

    assert_eq!(execution_account.as_deref(), Some("secondary-account"));
    assert_eq!(remote_control_account.as_deref(), Some("root-account"));
}
