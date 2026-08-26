use std::path::Path;
use std::path::PathBuf;

use chrono::Duration;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;

use super::restore_runtime_state;
use crate::AccountAvailability;
use crate::AccountPool;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountRuntimeProfileState;
use crate::AccountRuntimeState;
use crate::AuthKeyringBackendKind;
use crate::AuthManager;

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
