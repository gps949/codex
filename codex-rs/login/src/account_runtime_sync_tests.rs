use chrono::Duration;
use pretty_assertions::assert_eq;

use super::*;
use crate::AccountLease;
use crate::AccountProfile;
use crate::AuthManager;
use crate::CodexAuth;

#[derive(Clone, Copy)]
enum SyncEntry {
    Blocking,
    Nonblocking,
}

impl SyncEntry {
    fn synchronize(self, store: &AccountRuntimeStateStore, pool: &AccountPool) {
        match self {
            Self::Blocking => store.synchronize(pool).unwrap(),
            Self::Nonblocking => assert!(store.try_synchronize(pool).unwrap()),
        }
    }
}

struct Fixture {
    _home: tempfile::TempDir,
    pool: AccountPool,
    store: AccountRuntimeStateStore,
    first: AccountProfileId,
    second: AccountProfileId,
    reset: DateTime<Utc>,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let pool = AccountPool::new();
        let first = AccountProfileId::new("first").unwrap();
        let second = AccountProfileId::new("second").unwrap();
        for (priority, id) in [(0, &first), (1, &second)] {
            pool.register(
                AccountProfile::new(
                    id.clone(),
                    home.path().join(id.as_str()),
                    priority,
                    /*label*/ None,
                ),
                AuthManager::from_auth_for_testing(
                    CodexAuth::create_dummy_chatgpt_auth_for_testing(),
                ),
            )
            .unwrap();
        }
        pool.activate(&first).unwrap();
        let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
        store.save_pool(&pool).unwrap();
        Self {
            _home: home,
            pool,
            store,
            first,
            second,
            reset: Utc::now() + Duration::hours(2),
        }
    }

    fn exhaust_both(&self) -> AccountLease {
        let first = self.pool.lease().unwrap();
        self.pool.mark_exhausted(&first, Some(self.reset)).unwrap();
        let second = self.pool.lease().unwrap();
        self.pool.mark_exhausted(&second, Some(self.reset)).unwrap();
        self.pool.identity_lease().unwrap();
        self.store.save_pool(&self.pool).unwrap();
        second
    }
}

#[test]
fn immediate_sync_preserves_external_force_probe_selection_and_cooldown_clear() {
    for entry in [SyncEntry::Blocking, SyncEntry::Nonblocking] {
        for select_first in [false, true] {
            let fixture = Fixture::new();
            fixture.exhaust_both();
            let selected = if select_first {
                &fixture.first
            } else {
                &fixture.second
            };
            let mut expected_pool = fixture.pool.snapshots();
            for snapshot in &mut expected_pool {
                snapshot.is_active = &snapshot.profile.id == selected;
                if snapshot.is_active {
                    snapshot.availability = AccountAvailability::Available;
                    snapshot.backend_resets_at = None;
                }
            }
            fixture
                .store
                .select(selected.clone(), AccountSelectionMode::ForceProbe)
                .unwrap();
            let forced = fixture.store.load().unwrap();

            entry.synchronize(&fixture.store, &fixture.pool);

            assert_eq!(fixture.store.load().unwrap(), forced);
            assert_eq!(fixture.pool.snapshots(), expected_pool);
            assert_eq!(&fixture.pool.lease().unwrap().profile().id, selected);
        }
    }
}

#[test]
fn immediate_sync_keeps_a_request_bound_refusal_received_after_external_force_probe() {
    for entry in [SyncEntry::Blocking, SyncEntry::Nonblocking] {
        let fixture = Fixture::new();
        let outstanding = fixture.exhaust_both();
        fixture
            .store
            .select(fixture.second.clone(), AccountSelectionMode::ForceProbe)
            .unwrap();
        let forced = fixture.store.load().unwrap();
        fixture
            .pool
            .mark_exhausted(&outstanding, Some(fixture.reset))
            .unwrap();

        entry.synchronize(&fixture.store, &fixture.pool);

        let saved = fixture.store.load().unwrap();
        let refusal = saved
            .profiles
            .iter()
            .find(|profile| profile.profile_id == fixture.second)
            .unwrap();
        let forced_profile = forced
            .profiles
            .iter()
            .find(|profile| profile.profile_id == fixture.second)
            .unwrap();
        assert!(refusal.quota_failure_at > forced_profile.quota_failure_at);
        let mut expected = forced;
        let expected_profile = expected
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id == fixture.second)
            .unwrap();
        expected_profile.exhausted_until = Some(fixture.reset);
        expected_profile.backend_resets_at = Some(fixture.reset);
        expected_profile.quota_failure_at = refusal.quota_failure_at;
        assert_eq!(saved, expected);
        assert_eq!(
            fixture
                .pool
                .snapshots()
                .into_iter()
                .map(|snapshot| (snapshot.availability, snapshot.is_active))
                .collect::<Vec<_>>(),
            vec![
                (
                    AccountAvailability::Exhausted {
                        resets_at: Some(fixture.reset),
                    },
                    false,
                ),
                (
                    AccountAvailability::Exhausted {
                        resets_at: Some(fixture.reset),
                    },
                    true,
                ),
            ]
        );
    }
}

#[test]
fn immediate_import_and_stale_background_sync_keep_subsequent_local_failover() {
    for entry in [SyncEntry::Blocking, SyncEntry::Nonblocking] {
        for background_first in [false, true] {
            let fixture = Fixture::new();
            let second = fixture.pool.activate(&fixture.second).unwrap();
            fixture
                .pool
                .mark_exhausted(&second, Some(fixture.reset))
                .unwrap();
            fixture.store.save_pool(&fixture.pool).unwrap();
            let mut background_previous = fixture.store.load().unwrap();
            fixture
                .store
                .select(fixture.second.clone(), AccountSelectionMode::ForceProbe)
                .unwrap();
            let mut expected = fixture.store.load().unwrap();
            entry.synchronize(&fixture.store, &fixture.pool);
            let forced_lease = fixture.pool.lease().unwrap();
            assert_eq!(forced_lease.profile().id, fixture.second);
            fixture
                .pool
                .mark_exhausted(&forced_lease, Some(fixture.reset))
                .unwrap();
            assert_eq!(fixture.pool.lease().unwrap().profile().id, fixture.first);

            if background_first {
                fixture
                    .store
                    .synchronize_pool(&fixture.pool, &mut background_previous)
                    .unwrap();
            } else {
                entry.synchronize(&fixture.store, &fixture.pool);
            }

            let saved = fixture.store.load().unwrap();
            expected.active_profile_id = Some(fixture.first.clone());
            let expected_second = expected
                .profiles
                .iter_mut()
                .find(|profile| profile.profile_id == fixture.second)
                .unwrap();
            expected_second.exhausted_until = Some(fixture.reset);
            expected_second.backend_resets_at = Some(fixture.reset);
            expected_second.quota_failure_at = saved
                .profiles
                .iter()
                .find(|profile| profile.profile_id == fixture.second)
                .unwrap()
                .quota_failure_at;
            assert_eq!(saved, expected);

            if background_first {
                entry.synchronize(&fixture.store, &fixture.pool);
            } else {
                fixture
                    .store
                    .synchronize_pool(&fixture.pool, &mut background_previous)
                    .unwrap();
            }

            assert_eq!(fixture.store.load().unwrap(), expected);
            assert_eq!(
                fixture.pool.identity_lease().unwrap().profile().id,
                fixture.first
            );
        }
    }
}

#[test]
fn failed_immediate_save_keeps_local_failover_pending_for_the_next_sync() {
    for entry in [SyncEntry::Blocking, SyncEntry::Nonblocking] {
        let fixture = Fixture::new();
        let second = fixture.pool.activate(&fixture.second).unwrap();
        fixture
            .pool
            .mark_exhausted(&second, Some(fixture.reset))
            .unwrap();
        fixture.store.save_pool(&fixture.pool).unwrap();
        fixture
            .store
            .select(fixture.second.clone(), AccountSelectionMode::ForceProbe)
            .unwrap();
        entry.synchronize(&fixture.store, &fixture.pool);
        let mut expected = fixture.store.load().unwrap();
        let forced_lease = fixture.pool.lease().unwrap();
        fixture
            .pool
            .mark_exhausted(&forced_lease, Some(fixture.reset))
            .unwrap();
        let temporary_path = fixture.store.codex_home.join(format!(
            ".{ACCOUNT_RUNTIME_STATE_FILE}.tmp-{}",
            std::process::id()
        ));
        fs::create_dir(&temporary_path).unwrap();

        match entry {
            SyncEntry::Blocking => assert!(fixture.store.synchronize(&fixture.pool).is_err()),
            SyncEntry::Nonblocking => {
                assert!(fixture.store.try_synchronize(&fixture.pool).is_err())
            }
        }

        assert_eq!(fixture.store.load().unwrap(), expected);
        fs::remove_dir(&temporary_path).unwrap();
        entry.synchronize(&fixture.store, &fixture.pool);

        let saved = fixture.store.load().unwrap();
        expected.active_profile_id = Some(fixture.first.clone());
        let expected_second = expected
            .profiles
            .iter_mut()
            .find(|profile| profile.profile_id == fixture.second)
            .unwrap();
        expected_second.exhausted_until = Some(fixture.reset);
        expected_second.backend_resets_at = Some(fixture.reset);
        expected_second.quota_failure_at = saved
            .profiles
            .iter()
            .find(|profile| profile.profile_id == fixture.second)
            .unwrap()
            .quota_failure_at;
        assert_eq!(saved, expected);
        assert_eq!(
            fixture.pool.identity_lease().unwrap().profile().id,
            fixture.first
        );
    }
}
