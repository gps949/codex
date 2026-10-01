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

#[tokio::test]
async fn protected_attempts_survive_request_generation_changes_and_need_only_confirmation() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    pool.update_rate_limits(&standby, idle_limits()).unwrap();
    let attempted_at = Utc::now() - Duration::minutes(4);
    for mut observation in [
        WindowWarmupObservation::in_progress(attempted_at),
        WindowWarmupObservation::unconfirmed(attempted_at),
        WindowWarmupObservation::current(
            WindowWarmupOutcome::Succeeded,
            attempted_at,
            /*retry_after*/ None,
            /*consecutive_failures*/ 0,
        ),
    ] {
        observation.request_generation = 0;
        let needs_confirmation = observation.phase.is_some();
        pool.record_window_warmup(&standby, observation).unwrap();
        assert_eq!(
            pool.window_warmup_candidates(),
            Vec::<AccountProfileId>::new()
        );
        assert_eq!(
            pool.window_warmup_confirmation_candidates(),
            if needs_confirmation {
                vec![standby.clone()]
            } else {
                vec![]
            }
        );
    }
}

#[tokio::test]
async fn interrupted_claim_settles_and_future_dated_attempt_does_not_become_permanent() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let now = Utc::now();
    pool.record_window_warmup(&standby, WindowWarmupObservation::in_progress(now))
        .unwrap();
    assert_eq!(
        pool.window_warmup_confirmation_candidates(),
        Vec::<AccountProfileId>::new()
    );
    assert_eq!(
        pool.window_warmup_candidates(),
        Vec::<AccountProfileId>::new()
    );

    // A reset is an authoritative end to the previous attempt; a new attempt is independent.
    pool.apply_quota_reset(&standby, now + Duration::seconds(1))
        .unwrap();
    pool.record_window_warmup(
        &standby,
        WindowWarmupObservation::in_progress(now + Duration::hours(24)),
    )
    .unwrap();
    assert_eq!(pool.window_warmup_candidates(), vec![standby]);
}

#[test]
fn expired_attempt_protection_ends_even_with_a_future_retry_deadline() {
    let now = Utc::now();
    let mut observation = WindowWarmupObservation::unconfirmed(now - Duration::hours(6));
    observation.retry_after = Some(now + Duration::days(365));
    assert!(!observation.request_is_protected(&AccountRateLimits::default(), now));
}

#[tokio::test]
async fn legacy_completion_imports_as_unconfirmed_and_later_positive_quota_confirms_it() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let attempted_at = Utc::now() - Duration::minutes(1);
    let legacy = WindowWarmupObservation::current(
        WindowWarmupOutcome::Failed,
        attempted_at,
        Some(attempted_at + Duration::hours(5)),
        /*consecutive_failures*/ 0,
    );
    pool.update_rate_limits(&standby, idle_limits()).unwrap();
    pool.record_window_warmup(&standby, legacy).unwrap();
    assert_eq!(
        pool.window_warmup_confirmation_candidates(),
        vec![standby.clone()]
    );
    let mut started = idle_limits();
    started.primary.as_mut().unwrap().used_percent = 0.1;
    pool.update_rate_limits(&standby, started).unwrap();
    assert_eq!(
        pool.snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == standby)
            .unwrap()
            .window_warmup,
        Some(WindowWarmupObservation::current(
            WindowWarmupOutcome::Succeeded,
            attempted_at,
            /*retry_after*/ None,
            /*consecutive_failures*/ 0,
        ))
    );
    assert_eq!(
        pool.window_warmup_confirmation_candidates(),
        Vec::<AccountProfileId>::new()
    );
}

#[tokio::test]
async fn stale_unknown_reset_and_invalid_future_quota_allow_read_only_discovery() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let now = Utc::now();
    let stale = AccountRateLimits {
        secondary: Some(AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: None,
            window_minutes: Some(10080),
        }),
        observed_at: Some(now - Duration::minutes(31)),
        ..AccountRateLimits::default()
    };
    pool.update_rate_limits(&standby, stale.clone()).unwrap();
    assert_eq!(pool.window_warmup_candidates(), vec![standby.clone()]);
    pool.update_rate_limits(
        &standby,
        AccountRateLimits {
            observed_at: Some(now),
            ..stale
        },
    )
    .unwrap();
    assert_eq!(
        pool.window_warmup_candidates(),
        Vec::<AccountProfileId>::new()
    );
    pool.apply_quota_reset(&standby, now + Duration::seconds(1))
        .unwrap();
    pool.update_rate_limits(
        &standby,
        AccountRateLimits {
            primary: Some(AccountRateLimitWindow {
                used_percent: 10.0,
                resets_at: Some(now + Duration::days(1000)),
                window_minutes: Some(300),
            }),
            observed_at: Some(now + Duration::seconds(2)),
            ..AccountRateLimits::default()
        },
    )
    .unwrap();
    assert_eq!(pool.window_warmup_candidates(), vec![standby]);
}

#[tokio::test]
async fn replayed_reset_preserves_newer_quota_and_warmup_and_import_clears_old_epoch() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let reset_at = Utc::now() - Duration::minutes(2);
    let observation = WindowWarmupObservation::unconfirmed(reset_at + Duration::minutes(1));
    pool.record_window_warmup(
        &standby,
        WindowWarmupObservation::in_progress(reset_at - Duration::minutes(1)),
    )
    .unwrap();
    store.record_quota_reset(&standby, reset_at).unwrap();
    store.record_rate_limits(&standby, idle_limits()).unwrap();
    store
        .record_window_warmup(&standby, observation.clone())
        .unwrap();
    let expected = store.load().unwrap();
    store.record_quota_reset(&standby, reset_at).unwrap();
    assert_eq!(store.load().unwrap(), expected);
    store.apply_window_warmup_to_pool(&pool).unwrap();
    let snapshot = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id == standby)
        .unwrap();
    let mut expected_snapshot = snapshot.clone();
    expected_snapshot.quota_reset_at = Some(reset_at);
    expected_snapshot.window_warmup = Some(observation);
    assert_eq!(snapshot, expected_snapshot);
}

#[test]
fn busy_pool_lock_is_nonblocking_and_released_on_drop() {
    let home = TempDir::new().unwrap();
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let first = store.try_lock_window_warmup().unwrap().unwrap();
    assert!(store.try_lock_window_warmup().unwrap().is_none());
    drop(first);
    assert!(store.try_lock_window_warmup().unwrap().is_some());
}

#[test]
fn pending_and_disabled_profile_warmup_writes_do_not_create_runtime_state() {
    let home = TempDir::new().unwrap();
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let pending = profiles
        .allocate_profile(/*label*/ None, /*priority*/ 0)
        .unwrap();
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    assert!(matches!(
        store.record_window_warmup(
            &pending.id,
            WindowWarmupObservation::in_progress(Utc::now())
        ),
        Err(crate::AccountRuntimeStateError::UnavailableProfile(_))
    ));
    assert_eq!(store.load().unwrap(), AccountRuntimeState::default());
    profiles.complete_profile(&pending.id).unwrap();
    profiles
        .update_profile_metadata(
            &pending.id,
            crate::AccountProfileMetadataUpdate {
                disabled: Some(true),
                ..crate::AccountProfileMetadataUpdate::default()
            },
        )
        .unwrap();
    assert!(matches!(
        store.record_window_warmup(
            &pending.id,
            WindowWarmupObservation::in_progress(Utc::now())
        ),
        Err(crate::AccountRuntimeStateError::UnavailableProfile(_))
    ));
    assert_eq!(store.load().unwrap(), AccountRuntimeState::default());
}

#[tokio::test]
async fn durable_current_attempt_wins_over_a_future_attempt_in_stale_process() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let future = WindowWarmupObservation::in_progress(Utc::now() + Duration::hours(24));
    pool.record_window_warmup(&standby, future).unwrap();
    store.save_pool(&pool).unwrap();
    let mut previous = store.load().unwrap();
    let attempted_at = Utc::now();
    assert!(store.claim_window_warmup(&standby, attempted_at).unwrap());
    let completed = WindowWarmupObservation::unconfirmed(attempted_at);
    store
        .record_window_warmup(&standby, completed.clone())
        .unwrap();
    // The stale process changed its future record while the new owner completed.
    pool.record_window_warmup(
        &standby,
        WindowWarmupObservation::unconfirmed(Utc::now() + Duration::hours(25)),
    )
    .unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.profile_id == standby)
            .unwrap()
            .window_warmup,
        Some(completed)
    );
}

#[tokio::test]
async fn durable_send_claim_cannot_downgrade_a_final_phase_of_the_same_attempt() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    store.save_pool(&pool).unwrap();
    for outcome in [WindowWarmupOutcome::Failed, WindowWarmupOutcome::Succeeded] {
        let attempted_at = Utc::now();
        assert!(store.claim_window_warmup(&standby, attempted_at).unwrap());
        assert!(store.claim_window_warmup(&standby, attempted_at).unwrap());
        let final_phase = if outcome == WindowWarmupOutcome::Failed {
            WindowWarmupObservation::unconfirmed(attempted_at)
        } else {
            WindowWarmupObservation::current(
                outcome,
                attempted_at,
                /*retry_after*/ None,
                /*consecutive_failures*/ 0,
            )
        };
        store.record_window_warmup(&standby, final_phase).unwrap();
        let completed = store.load().unwrap();
        assert!(!store.claim_window_warmup(&standby, attempted_at).unwrap());
        assert_eq!(store.load().unwrap(), completed);
        store.record_quota_reset(&standby, Utc::now()).unwrap();
    }
}

#[tokio::test]
async fn entitlement_reset_credit_exclusion_survives_pool_publication_and_restart() {
    let home = TempDir::new().unwrap();
    let (pool, standby) = standby_pool(&home).await;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    store.save_pool(&pool).unwrap();
    let deadline = Utc::now() + Duration::minutes(9);
    store
        .exclude_reset_credit_until(&standby, deadline)
        .unwrap();
    let mut previous = store.load().unwrap();
    store.synchronize_pool(&pool, &mut previous).unwrap();
    store.save_pool(&pool).unwrap();
    assert_eq!(
        store
            .load()
            .unwrap()
            .profiles
            .iter()
            .find(|profile| profile.profile_id == standby)
            .unwrap()
            .reset_credit_excluded_until,
        Some(deadline)
    );
}
