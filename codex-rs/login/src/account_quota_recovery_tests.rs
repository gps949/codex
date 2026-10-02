use super::*;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountRuntimeStateStore;
use crate::AuthManager;
use crate::CodexAuth;
use pretty_assertions::assert_eq;

#[test]
fn recovery_provenance_survives_restart_and_force_probe_clears_it() {
    let home = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let retry_at = now + chrono::Duration::hours(2);
    for (recovery, expected_backend) in [
        (
            AccountQuotaRecovery::BackendReset {
                resets_at: retry_at,
                retry_at,
            },
            Some(retry_at),
        ),
        (AccountQuotaRecovery::Reprobe { retry_at }, None),
    ] {
        let pool = AccountPool::new();
        let profile = AccountProfile::new(
            AccountProfileId::new("fixture").unwrap(),
            home.path().join("profile"),
            /*priority*/ 0,
            /*label*/ None,
        );
        pool.register(
            profile.clone(),
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
        )
        .unwrap();
        let lease = pool.activate(&profile.id).unwrap();
        pool.mark_exhausted_for_recovery(&lease, recovery, /*rate_limits*/ None)
            .unwrap();
        let snapshot = pool.snapshots().remove(0);
        assert_eq!(
            (snapshot.availability, snapshot.backend_resets_at),
            (
                AccountAvailability::Exhausted {
                    resets_at: Some(retry_at)
                },
                expected_backend,
            )
        );
        let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
        store.save_pool(&pool).unwrap();
        let persisted = store.load().unwrap().profiles.remove(0);
        assert_eq!(
            (persisted.exhausted_until, persisted.backend_resets_at),
            (Some(retry_at), expected_backend)
        );
        pool.force_activate(&profile.id).unwrap();
        assert_eq!(pool.snapshots().remove(0).backend_resets_at, None);
    }
}

#[test]
fn stale_recovery_failure_cannot_install_backend_reset_on_a_new_lease() {
    let pool = AccountPool::new();
    let id = AccountProfileId::new("fixture").unwrap();
    pool.register(
        AccountProfile::new(
            id.clone(),
            "fixture".into(),
            /*priority*/ 0,
            /*label*/ None,
        ),
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    )
    .unwrap();
    let old = pool.activate(&id).unwrap();
    pool.mark_exhausted(&old, Some(Utc::now() + chrono::Duration::hours(1)))
        .unwrap();
    let new = pool.force_activate(&id).unwrap();
    assert!(new.generation() > old.generation());
    let reset = Utc::now() + chrono::Duration::hours(2);
    pool.mark_exhausted_for_recovery(
        &old,
        AccountQuotaRecovery::BackendReset {
            resets_at: reset,
            retry_at: reset,
        },
        /*rate_limits*/ None,
    )
    .unwrap();
    let snapshot = pool.snapshots().remove(0);
    assert_eq!(
        (snapshot.availability, snapshot.backend_resets_at),
        (AccountAvailability::Available, None)
    );
}
