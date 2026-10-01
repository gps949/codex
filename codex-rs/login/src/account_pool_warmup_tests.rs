use chrono::Duration;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::AccountProfileStore;
use crate::AccountRateLimitWindow;
use crate::AccountRuntimeState;
use crate::AccountRuntimeStateStore;
use crate::AuthKeyringBackendKind;
use crate::AuthManager;

async fn standby_pool(home: &TempDir) -> (AccountPool, AccountProfileId) {
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let pool = AccountPool::new();
    let mut standby = None;
    for priority in [0, 10] {
        let profile = profiles.allocate_profile(/*label*/ None, priority).unwrap();
        profiles.complete_profile(&profile.id).unwrap();
        let manager = AuthManager::shared(
            profile.credential_home.clone(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            crate::test_support::transport_default_auth_route_config(),
        )
        .await;
        let id = profile.id.clone();
        pool.register(profile, manager).unwrap();
        if priority == 0 {
            pool.activate(&id).unwrap();
        } else {
            standby = Some(id);
        }
    }
    (pool, standby.unwrap())
}

fn idle_limits() -> AccountRateLimits {
    let now = Utc::now();
    AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    }
}

#[tokio::test]
async fn completed_phase_wins_equal_attempt_in_memory_disk_and_three_way_merge() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    pool.update_rate_limits(&standby, idle_limits()).unwrap();
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    store.save_pool(&pool).unwrap();
    let mut previous = store.load().unwrap();
    let attempted_at = Utc::now() - Duration::minutes(1);
    let pending = WindowWarmupObservation::in_progress(attempted_at);
    let completed = WindowWarmupObservation::unconfirmed(attempted_at);
    pool.record_window_warmup(&standby, pending.clone())
        .unwrap();
    store
        .record_window_warmup(&standby, completed.clone())
        .unwrap();

    store.synchronize_pool(&pool, &mut previous).unwrap();
    let expected = store.load().unwrap();
    assert_eq!(
        expected
            .profiles
            .iter()
            .find(|profile| profile.profile_id == standby)
            .unwrap()
            .window_warmup,
        Some(completed.clone())
    );
    pool.record_window_warmup(&standby, pending.clone())
        .unwrap();
    store.record_window_warmup(&standby, pending).unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    assert_eq!(store.load().unwrap(), expected);
    assert_eq!(
        pool.snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == standby)
            .unwrap()
            .window_warmup,
        Some(completed)
    );
}
