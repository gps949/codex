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
        selection_revision: 0,
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
    store
        .ensure_legacy_root_profile(Some("Root".to_string()), /*priority*/ 0)
        .unwrap();
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
        .runtime_state_store()
        .select(
            secondary.id.clone(),
            crate::AccountSelectionMode::AvailableOnly,
        )
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if execution_auth_manager
                .auth()
                .await
                .and_then(|auth| auth.get_account_id())
                .as_deref()
                == Some("secondary-account")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("running runtime must observe the external selection");

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

#[tokio::test]
async fn running_pool_observes_external_selection_without_losing_cooldowns() {
    let home = TempDir::new().unwrap();
    let pool = AccountPool::new();
    let manager = test_auth_manager(home.path()).await;
    pool.register(profile("first", 0), Arc::clone(&manager))
        .unwrap();
    pool.register(profile("second", 1), manager).unwrap();
    let first = AccountProfileId::new("first").unwrap();
    let second = AccountProfileId::new("second").unwrap();
    pool.activate(&first).unwrap();
    let store = crate::AccountRuntimeStateStore::new(home.path().to_path_buf());
    store.save_pool(&pool).unwrap();
    let mut previous = store.load().unwrap();
    let mut selected = previous.clone();
    selected.active_profile_id = Some(second.clone());
    store.save(&selected).unwrap();
    // A local observation arrives after another process has selected second.
    pool.update_rate_limits(
        &first,
        crate::AccountRateLimits {
            observed_at: Some(Utc::now()),
            ..Default::default()
        },
    )
    .unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    assert_eq!(pool.lease().unwrap().profile().id, second);
    assert_eq!(store.load().unwrap().active_profile_id, Some(second));
}

#[tokio::test]
async fn stale_pool_does_not_undo_selection_of_a_new_profile() {
    let home = TempDir::new().unwrap();
    let pool = AccountPool::new();
    pool.register(profile("first", 0), test_auth_manager(home.path()).await)
        .unwrap();
    pool.lease().unwrap();
    let store = crate::AccountRuntimeStateStore::new(home.path().to_path_buf());
    let mut previous = AccountRuntimeState::default();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    let second = AccountProfileId::new("new-profile").unwrap();
    store
        .select(second.clone(), crate::AccountSelectionMode::AvailableOnly)
        .unwrap();
    for _ in 0..2 {
        store.synchronize_pool(&pool, &mut previous).unwrap();
        assert_eq!(
            store.load().unwrap().active_profile_id,
            Some(second.clone())
        );
    }
}

#[tokio::test]
async fn two_pools_merge_quota_observations_and_force_selection() {
    let home = TempDir::new().unwrap();
    let first = AccountProfileId::new("first").unwrap();
    let second = AccountProfileId::new("second").unwrap();
    let pools = [AccountPool::new(), AccountPool::new()];
    let mut previous = [
        AccountRuntimeState::default(),
        AccountRuntimeState::default(),
    ];
    let store = crate::AccountRuntimeStateStore::new(home.path().to_path_buf());
    for (pool, previous) in pools.iter().zip(&mut previous) {
        let manager = test_auth_manager(home.path()).await;
        pool.register(profile("first", 0), Arc::clone(&manager))
            .unwrap();
        pool.register(profile("second", 1), manager).unwrap();
        pool.activate(&first).unwrap();
        store.synchronize_pool(pool, previous).unwrap();
    }
    let reset = Utc::now() + Duration::hours(1);
    let lease = pools[0].lease().unwrap();
    pools[0].mark_exhausted(&lease, Some(reset)).unwrap();
    store.synchronize_pool(&pools[0], &mut previous[0]).unwrap();
    let limits = crate::AccountRateLimits {
        observed_at: Some(Utc::now()),
        ..Default::default()
    };
    pools[1]
        .update_rate_limits(&second, limits.clone())
        .unwrap();
    store.synchronize_pool(&pools[1], &mut previous[1]).unwrap();
    let disk = store.load().unwrap();
    assert_eq!(disk.active_profile_id, Some(second.clone()));
    assert_eq!(
        disk.profiles
            .iter()
            .find(|p| p.profile_id == first)
            .unwrap()
            .exhausted_until,
        Some(reset)
    );
    assert_eq!(
        disk.profiles
            .iter()
            .find(|p| p.profile_id == second)
            .unwrap()
            .rate_limits,
        limits
    );
    assert!(
        store
            .select(first.clone(), crate::AccountSelectionMode::AvailableOnly)
            .is_err()
    );
    store
        .select(first.clone(), crate::AccountSelectionMode::ForceProbe)
        .unwrap();
    for _ in 0..2 {
        for (pool, previous) in pools.iter().zip(&mut previous) {
            store.synchronize_pool(pool, previous).unwrap();
            assert_eq!(pool.lease().unwrap().profile().id, first);
        }
    }
    assert_eq!(
        store
            .load()
            .unwrap()
            .profiles
            .iter()
            .find(|p| p.profile_id == first)
            .unwrap()
            .exhausted_until,
        None
    );
}

#[tokio::test]
async fn running_pool_applies_external_disable_and_metadata_updates() {
    let home = TempDir::new().unwrap();
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let first = profiles
        .allocate_profile(Some("Original".to_string()), 0)
        .unwrap();
    profiles.complete_profile(&first.id).unwrap();
    let pool = AccountPool::new();
    pool.register(first.clone(), test_auth_manager(home.path()).await)
        .unwrap();
    pool.lease().unwrap();
    let store = crate::AccountRuntimeStateStore::new(home.path().to_path_buf());
    let mut previous = AccountRuntimeState::default();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    profiles
        .update_profile_metadata(
            &first.id,
            crate::AccountProfileMetadataUpdate {
                label: Some(crate::AccountLabelUpdate::Set("Renamed".to_string())),
                disabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    assert!(pool.lease().is_err());
    assert_eq!(
        pool.snapshots()[0].profile.label.as_deref(),
        Some("Renamed")
    );
    assert!(
        store
            .select(first.id.clone(), crate::AccountSelectionMode::ForceProbe)
            .is_err()
    );
    profiles.remove_profile_metadata(&first.id).unwrap();
    store.remove_profile(&first.id).unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    assert!(pool.snapshots().is_empty());
    assert!(store.load().unwrap().profiles.is_empty());
}

#[tokio::test]
async fn explicitly_removed_root_profile_stays_out_of_pool_after_restart() {
    let home = TempDir::new().unwrap();
    save_auth(
        home.path(),
        &chatgpt_auth("root"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let root = profiles
        .ensure_legacy_root_profile(Some("Original".to_string()), 20)
        .unwrap();
    let managed = profiles
        .allocate_profile(Some("Managed".to_string()), 0)
        .unwrap();
    save_auth(
        &managed.credential_home,
        &chatgpt_auth("managed"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    profiles.complete_profile(&managed.id).unwrap();
    profiles.remove_profile_metadata(&root.id).unwrap();
    let runtime = AccountPoolRuntime::install(
        test_auth_manager(home.path()).await,
        test_auth_config(home.path().to_path_buf()),
        /*include_existing_root_login*/ true,
    )
    .await
    .unwrap();
    assert_eq!(
        runtime
            .pool()
            .snapshots()
            .iter()
            .map(|account| account.profile.id.clone())
            .collect::<Vec<_>>(),
        vec![managed.id]
    );
    assert!(home.path().join("auth.json").exists());
}
