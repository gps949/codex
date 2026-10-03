use base64::Engine as _;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;

use super::*;
use crate::AccountLease;
use crate::AccountProfileStore;
use crate::AccountRateLimitWindow;
use crate::AuthManager;
use crate::CodexAuth;
use crate::auth::AuthDotJson;
use crate::auth::AuthKeyringBackendKind;
use codex_config::types::AuthCredentialsStoreMode;

struct Fixture {
    _home: tempfile::TempDir,
    pool: AccountPool,
    store: AccountRuntimeStateStore,
    id: AccountProfileId,
    auth: CodexAuth,
    failed: AccountLease,
    reset: DateTime<Utc>,
}

fn fixture_auth(user: &str) -> CodexAuth {
    let claims = serde_json::json!({"https://api.openai.com/auth": {
        "chatgpt_user_id": user, "chatgpt_account_id": "fixture-account", "chatgpt_plan_type": "pro",
    }});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    CodexAuth::from_external_chatgpt_tokens(
        &format!("header.{payload}.signature"),
        "fixture-account",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap()
}

fn save_fixture_auth(home: &std::path::Path, auth: &CodexAuth) {
    crate::account_credentials::save_login_auth(
        home,
        &AuthDotJson {
            auth_mode: Some(auth.api_auth_mode()),
            openai_api_key: None,
            tokens: Some(auth.get_token_data().unwrap()),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        },
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let profile = profiles
            .allocate_profile(/*label*/ None, /*priority*/ 0)
            .unwrap();
        let auth = fixture_auth("fixture-owner");
        save_fixture_auth(&profile.credential_home, &auth);
        profiles.complete_profile(&profile.id).unwrap();
        let pool = AccountPool::new();
        pool.register(
            profile.clone(),
            AuthManager::from_auth_for_testing_with_home(
                auth.clone(),
                profile.credential_home.clone(),
            ),
        )
        .unwrap();
        let failed = pool.activate(&profile.id).unwrap();
        let reset = Utc::now() + chrono::Duration::hours(2);
        let window = AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(reset),
            window_minutes: Some(300),
        };
        pool.mark_exhausted_with_rate_limits(
            &failed,
            Some(reset),
            AccountRateLimits {
                primary: Some(window.clone()),
                secondary: Some(window),
                observed_at: Some(Utc::now()),
                window_observed_at: None,
            },
        )
        .unwrap();
        let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
        store.save_pool(&pool).unwrap();
        Self {
            _home: home,
            pool,
            store,
            id: profile.id,
            auth,
            failed,
            reset,
        }
    }

    fn snapshot(&self, reset: DateTime<Utc>) -> RateLimitSnapshot {
        let window = RateLimitWindow {
            used_percent: 0.0,
            window_minutes: Some(300),
            resets_at: Some(reset.timestamp()),
        };
        RateLimitSnapshot {
            limit_id: Some("codex".into()),
            limit_name: None,
            normal_model_slug: None,
            primary: Some(window.clone()),
            secondary: Some(window),
            credits: None,
            individual_limit: None,
            spend_control_reached: Some(false),
            plan_type: None,
            rate_limit_reached_type: None,
        }
    }

    fn probe(&self) -> AccountQuotaProbe {
        self.store
            .capture_quota_probe(&self.pool, &self.id, &self.auth)
            .unwrap()
            .unwrap()
    }

    fn commit(&self, probe: AccountQuotaProbe, snapshot: RateLimitSnapshot) -> bool {
        self.store
            .reconcile_quota_probe(
                &self.pool,
                probe,
                AccountQuotaEvidence {
                    rate_limits: &[snapshot],
                    ordinary_usage_allowed: Some(true),
                    account_id: Some("fixture-account"),
                    user_id: Some("fixture-owner"),
                },
            )
            .unwrap()
    }
}

#[test]
fn confirmed_probe_replaces_same_or_earlier_reset_quota_and_recovers_cooldown() {
    for earlier in [chrono::Duration::zero(), chrono::Duration::minutes(5)] {
        let fixture = Fixture::new();
        let before = fixture.store.load().unwrap();
        assert!(fixture.commit(fixture.probe(), fixture.snapshot(fixture.reset - earlier)));
        let snapshot = fixture.pool.snapshots().remove(0);
        let saved = fixture.store.load().unwrap();
        assert_eq!(
            (
                snapshot.availability,
                snapshot.backend_resets_at,
                saved.profiles[0].exhausted_until
            ),
            (AccountAvailability::Available, None, None)
        );
        assert_eq!(snapshot.rate_limits, saved.profiles[0].rate_limits);
        assert_eq!(snapshot.rate_limits.primary.unwrap().used_percent, 0.0);
        assert_eq!(
            (saved.active_profile_id, saved.selection_revision),
            (before.active_profile_id, before.selection_revision)
        );
        assert!(fixture.pool.lease().is_ok());
    }
}

#[test]
fn identical_new_refusal_blocks_an_inflight_recovery_probe() {
    let fixture = Fixture::new();
    let probe = fixture.probe();
    fixture
        .pool
        .mark_exhausted(&fixture.failed, Some(fixture.reset))
        .unwrap();
    let before = fixture.pool.snapshots();
    assert!(!fixture.commit(probe, fixture.snapshot(fixture.reset)));
    assert_eq!(fixture.pool.snapshots(), before);
}

#[test]
fn persisted_failure_entitlement_and_credential_changes_block_late_recovery() {
    for change in [
        "failure",
        "entitlement",
        "credentials",
        "reset",
        "live_reset",
        "disabled",
        "disabled_then_enabled",
        "auth",
        "incarnation",
    ] {
        let fixture = Fixture::new();
        let probe = fixture.probe();
        match change {
            "failure" => {
                let mut saved = fixture.store.load().unwrap();
                saved.profiles[0].quota_failure_at = Some(Utc::now());
                fixture.store.save(&saved).unwrap();
            }
            "entitlement" => fixture
                .store
                .exclude_reset_credit_until(&fixture.id, fixture.reset)
                .unwrap(),
            "credentials" => {
                let home = fixture.pool.snapshots()[0].profile.credential_home.clone();
                std::fs::write(home.join(".account-credentials-version"), "new-owner").unwrap();
            }
            "reset" => fixture
                .store
                .record_quota_reset(&fixture.id, Utc::now())
                .unwrap(),
            "live_reset" => fixture.pool.reset_rate_limits(&fixture.id).unwrap(),
            "disabled" => fixture.pool.set_disabled(&fixture.id, true).unwrap(),
            "disabled_then_enabled" => {
                let profiles = AccountProfileStore::new(fixture.store.codex_home.clone());
                for disabled in [true, false] {
                    profiles
                        .update_profile_metadata(
                            &fixture.id,
                            crate::AccountProfileMetadataUpdate {
                                disabled: Some(disabled),
                                ..crate::AccountProfileMetadataUpdate::default()
                            },
                        )
                        .unwrap();
                }
            }
            "auth" => {
                fixture
                    .pool
                    .mark_authentication_unavailable(&fixture.failed, "synthetic auth refusal")
                    .unwrap();
            }
            "incarnation" => {
                let profile = fixture.pool.snapshots()[0].profile.clone();
                fixture.pool.merge_runtime_state(
                    &AccountRuntimeState::default(),
                    &AccountRuntimeState::default(),
                    Some(&[]),
                );
                fixture
                    .pool
                    .register(
                        profile,
                        AuthManager::from_auth_for_testing(fixture_auth("new-owner")),
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = fixture.store.load().unwrap();
        assert!(
            !fixture.commit(probe, fixture.snapshot(fixture.reset)),
            "{change}"
        );
        assert_eq!(fixture.store.load().unwrap(), before);
    }
}

#[test]
fn busy_home_or_credentials_lock_skips_recovery_without_waiting() {
    for credentials in [false, true] {
        let fixture = Fixture::new();
        let probe = fixture.probe();
        let locked = if credentials {
            crate::account_file::refresh_lock(&fixture.pool.snapshots()[0].profile.credential_home)
                .unwrap()
        } else {
            crate::account_file::lock(&fixture.store.codex_home).unwrap()
        };
        assert!(
            fixture
                .store
                .capture_quota_probe(&fixture.pool, &fixture.id, &fixture.auth)
                .unwrap()
                .is_none()
        );
        assert!(!fixture.commit(probe, fixture.snapshot(fixture.reset)));
        drop(locked);
        assert!(fixture.commit(fixture.probe(), fixture.snapshot(fixture.reset)));
    }
}

#[test]
fn confirmed_credit_reset_uses_the_same_conflict_barrier() {
    for newer_refusal in [false, true] {
        let fixture = Fixture::new();
        let probe = fixture.probe();
        if newer_refusal {
            fixture
                .pool
                .mark_exhausted(&fixture.failed, Some(fixture.reset))
                .unwrap();
        }
        assert_eq!(
            fixture
                .store
                .confirm_quota_reset(&fixture.pool, probe, Utc::now())
                .unwrap(),
            !newer_refusal
        );
        assert_eq!(
            fixture.pool.snapshots()[0].availability,
            if newer_refusal {
                AccountAvailability::Exhausted {
                    resets_at: Some(fixture.reset),
                }
            } else {
                AccountAvailability::Available
            }
        );
    }
}

#[test]
fn failed_recovery_persistence_does_not_publish_live_availability() {
    let fixture = Fixture::new();
    let probe = fixture.probe();
    let before = (fixture.pool.snapshots(), fixture.store.load().unwrap());
    std::fs::create_dir(fixture.store.codex_home.join(format!(
        ".account-runtime-state.json.tmp-{}",
        std::process::id()
    )))
    .unwrap();
    let snapshot = fixture.snapshot(fixture.reset);
    assert!(
        fixture
            .store
            .reconcile_quota_probe(
                &fixture.pool,
                probe,
                AccountQuotaEvidence {
                    rate_limits: &[snapshot],
                    ordinary_usage_allowed: Some(true),
                    account_id: Some("fixture-account"),
                    user_id: Some("fixture-owner"),
                }
            )
            .is_err()
    );
    assert_eq!(
        (fixture.pool.snapshots(), fixture.store.load().unwrap()),
        before
    );
}

#[test]
fn partial_disallowed_malformed_or_other_owner_metadata_never_recovers() {
    for invalid in [
        "allowed",
        "allowed_false",
        "owner",
        "missing_owner",
        "primary",
        "secondary",
        "secondary_full",
        "percent",
        "reset",
        "duration",
        "spend",
        "bucket",
        "entitlement",
    ] {
        let fixture = Fixture::new();
        let probe = fixture.probe();
        let mut snapshot = fixture.snapshot(fixture.reset);
        match invalid {
            "primary" => snapshot.primary = None,
            "secondary" => snapshot.secondary = None,
            "secondary_full" => snapshot.secondary.as_mut().unwrap().used_percent = 100.0,
            "percent" => snapshot.primary.as_mut().unwrap().used_percent = f64::NAN,
            "reset" => snapshot.primary.as_mut().unwrap().resets_at = None,
            "duration" => snapshot.primary.as_mut().unwrap().window_minutes = None,
            "spend" => snapshot.spend_control_reached = Some(true),
            "bucket" => snapshot.limit_id = Some("other-model".into()),
            "entitlement" => {
                snapshot.rate_limit_reached_type = Some(
                    codex_protocol::protocol::RateLimitReachedType::WorkspaceOwnerCreditsDepleted,
                )
            }
            "allowed" | "allowed_false" | "owner" | "missing_owner" => {}
            _ => unreachable!(),
        }
        let before = fixture.pool.snapshots();
        assert!(
            !fixture
                .store
                .reconcile_quota_probe(
                    &fixture.pool,
                    probe,
                    AccountQuotaEvidence {
                        rate_limits: &[snapshot],
                        ordinary_usage_allowed: (invalid != "allowed")
                            .then_some(invalid != "allowed_false"),
                        account_id: Some("fixture-account"),
                        user_id: (invalid != "missing_owner").then_some(if invalid == "owner" {
                            "other-owner"
                        } else {
                            "fixture-owner"
                        }),
                    }
                )
                .unwrap(),
            "{invalid}"
        );
        assert_eq!(fixture.pool.snapshots(), before);
    }
}

#[test]
fn missing_secondary_window_does_not_invent_nonapplicability_for_unknown_quota() {
    let fixture = Fixture::new();
    fixture.pool.reset_rate_limits(&fixture.id).unwrap();
    let lease = fixture.pool.lease().unwrap();
    fixture
        .pool
        .mark_exhausted(&lease, Some(fixture.reset))
        .unwrap();
    fixture.store.save_pool(&fixture.pool).unwrap();
    let mut snapshot = fixture.snapshot(fixture.reset);
    snapshot.secondary = None;
    assert!(!fixture.commit(fixture.probe(), snapshot));
}

#[test]
fn shared_recovery_reaches_another_pool_but_newer_failure_wins() {
    let fixture = Fixture::new();
    let other = AccountPool::new();
    other
        .register(
            fixture.pool.snapshots()[0].profile.clone(),
            AuthManager::from_auth_for_testing(fixture.auth.clone()),
        )
        .unwrap();
    let mut previous = fixture.store.load().unwrap();
    other.merge_runtime_state(&previous, &AccountRuntimeState::default(), None);
    assert!(fixture.commit(fixture.probe(), fixture.snapshot(fixture.reset)));
    fixture
        .store
        .synchronize_pool(&other, &mut previous)
        .unwrap();
    assert_eq!(
        other.snapshots()[0].availability,
        AccountAvailability::Available
    );
    let lease = other.lease().unwrap();
    other.mark_exhausted(&lease, Some(fixture.reset)).unwrap();
    fixture
        .store
        .synchronize_pool(&other, &mut previous)
        .unwrap();
    fixture.store.synchronize(&fixture.pool).unwrap();
    assert_eq!(
        fixture.pool.snapshots()[0].availability,
        AccountAvailability::Exhausted {
            resets_at: Some(fixture.reset)
        }
    );
}

#[test]
fn pending_refusal_is_ordered_after_a_shared_recovery_when_clock_moves_back() {
    let fixture = Fixture::new();
    let mut previous = fixture.store.load().unwrap();
    let shared_reset = Utc::now() + chrono::Duration::minutes(1);
    let mut remote = previous.clone();
    remote.profiles[0].quota_reset_at = Some(shared_reset);
    remote.profiles[0].exhausted_until = None;
    remote.profiles[0].backend_resets_at = None;
    fixture.store.save(&remote).unwrap();
    fixture
        .pool
        .mark_exhausted(&fixture.failed, Some(fixture.reset))
        .unwrap();
    fixture
        .store
        .synchronize_pool(&fixture.pool, &mut previous)
        .unwrap();
    let saved = fixture.store.load().unwrap();
    assert_eq!(saved.profiles[0].exhausted_until, Some(fixture.reset));
    assert!(saved.profiles[0].quota_failure_at > Some(shared_reset));
}

#[test]
fn busy_shared_import_skips_without_publishing_or_acknowledging_a_refusal() {
    let fixture = Fixture::new();
    fixture
        .pool
        .mark_exhausted(&fixture.failed, Some(fixture.reset))
        .unwrap();
    let before = fixture.pool.snapshots();
    let lock = crate::account_file::lock(&fixture.store.codex_home).unwrap();
    assert!(!fixture.store.try_synchronize(&fixture.pool).unwrap());
    assert_eq!(fixture.pool.snapshots(), before);
    drop(lock);
    assert!(fixture.store.try_synchronize(&fixture.pool).unwrap());
    assert_eq!(
        fixture.store.load().unwrap().profiles[0].exhausted_until,
        Some(fixture.reset)
    );
    assert!(fixture.pool.snapshots()[0].quota_failure_at > before[0].quota_failure_at);
}

#[tokio::test]
async fn credentials_written_before_capture_must_match_the_resolved_request() {
    for same_owner in [false, true] {
        let fixture = Fixture::new();
        let user = if same_owner {
            "fixture-owner"
        } else {
            "new-owner"
        };
        let mut replacement = fixture_auth(user).get_token_data().unwrap();
        replacement.access_token.push_str("-new-login");
        let credential_home = fixture.pool.snapshots()[0].profile.credential_home.clone();
        let stored = AuthDotJson {
            auth_mode: Some(fixture.auth.api_auth_mode()),
            tokens: Some(replacement),
            openai_api_key: None,
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };
        crate::account_credentials::save_login_auth(
            &credential_home,
            &stored,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
        let before = (fixture.pool.snapshots(), fixture.store.load().unwrap());
        assert!(
            fixture
                .store
                .capture_quota_probe(&fixture.pool, &fixture.id, &fixture.auth)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            (fixture.pool.snapshots(), fixture.store.load().unwrap()),
            before
        );

        let manager = fixture.pool.auth_managers().remove(0).1;
        manager.reload().await;
        let current = manager.auth_cached().unwrap();
        let probe = fixture
            .store
            .capture_quota_probe(&fixture.pool, &fixture.id, &current)
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .store
                .reconcile_quota_probe(
                    &fixture.pool,
                    probe,
                    AccountQuotaEvidence {
                        rate_limits: &[fixture.snapshot(fixture.reset)],
                        ordinary_usage_allowed: Some(true),
                        account_id: Some("fixture-account"),
                        user_id: Some(user),
                    }
                )
                .unwrap()
        );
    }
}

#[test]
fn future_failure_recovery_retains_real_quota_times_and_accepts_later_observations() {
    let fixture = Fixture::new();
    let future_failure = Utc::now() + chrono::Duration::days(30);
    let mut state = fixture.store.load().unwrap();
    state.profiles[0].quota_failure_at = Some(future_failure);
    state.profiles[0].rate_limits.observed_at = Some(future_failure);
    fixture.store.save(&state).unwrap();
    fixture.store.synchronize(&fixture.pool).unwrap();

    let poisoned = fixture.pool.snapshots()[0].rate_limits.clone();
    let began = Utc::now();
    let mut usage = fixture.snapshot(began + chrono::Duration::hours(5));
    usage.secondary.as_mut().unwrap().window_minutes = Some(10_080);
    usage.secondary.as_mut().unwrap().resets_at =
        Some((began + chrono::Duration::days(7)).timestamp());
    assert!(fixture.commit(fixture.probe(), usage));
    let recovered = fixture.pool.snapshots().remove(0);
    assert!(recovered.quota_reset_at > Some(future_failure));
    assert!((Some(began)..=Some(Utc::now())).contains(&recovered.rate_limits.observed_at));
    assert!(recovered.quota_reset_observed_at < Some(future_failure));

    fixture
        .pool
        .update_rate_limits(&fixture.id, poisoned.clone())
        .unwrap();
    fixture
        .store
        .record_rate_limits(&fixture.id, poisoned)
        .unwrap();
    assert_eq!(
        fixture.pool.snapshots()[0].rate_limits,
        recovered.rate_limits
    );
    let mut later = recovered.rate_limits;
    later.primary.as_mut().unwrap().used_percent = 20.0;
    later.observed_at = Some(Utc::now());
    fixture
        .pool
        .update_rate_limits(&fixture.id, later.clone())
        .unwrap();
    fixture
        .store
        .record_rate_limits(&fixture.id, later.clone())
        .unwrap();
    fixture.store.synchronize(&fixture.pool).unwrap();
    assert_eq!(fixture.pool.snapshots()[0].rate_limits, later);
    assert_eq!(fixture.store.load().unwrap().profiles[0].rate_limits, later);

    let profile = fixture.pool.snapshots()[0].profile.clone();
    let other = AccountPool::new();
    other
        .register(
            profile.clone(),
            AuthManager::from_auth_for_testing_with_home(
                fixture.auth.clone(),
                profile.credential_home,
            ),
        )
        .unwrap();
    fixture.store.synchronize(&other).unwrap();
    assert_eq!(other.snapshots()[0].rate_limits, later);
    assert!(other.lease().is_ok());
}

#[test]
fn future_failure_confirmation_still_rejects_new_local_or_shared_refusals() {
    for operation in ["quota", "reset"] {
        for refusal in ["none", "local", "shared"] {
            let fixture = Fixture::new();
            let future_failure = Utc::now() + chrono::Duration::days(30);
            let mut state = fixture.store.load().unwrap();
            state.profiles[0].quota_failure_at = Some(future_failure);
            fixture.store.save(&state).unwrap();
            fixture.store.synchronize(&fixture.pool).unwrap();
            let probe = fixture.probe();
            match refusal {
                "none" => {}
                "local" => {
                    fixture
                        .pool
                        .mark_exhausted(&fixture.failed, Some(fixture.reset))
                        .unwrap();
                }
                "shared" => {
                    let mut state = fixture.store.load().unwrap();
                    state.profiles[0].quota_failure_at =
                        Some(future_failure + chrono::Duration::nanoseconds(1));
                    fixture.store.save(&state).unwrap();
                }
                _ => unreachable!(),
            }
            let before = (fixture.pool.snapshots(), fixture.store.load().unwrap());
            let confirmed = match operation {
                "quota" => fixture.commit(probe, fixture.snapshot(fixture.reset)),
                "reset" => fixture
                    .store
                    .confirm_quota_reset(&fixture.pool, probe, Utc::now())
                    .unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(confirmed, refusal == "none");
            if confirmed {
                let snapshot = fixture.pool.snapshots().remove(0);
                assert_eq!(snapshot.availability, AccountAvailability::Available);
                assert!(snapshot.quota_reset_at > Some(future_failure));
                assert!(snapshot.rate_limits.observed_at <= Some(Utc::now()));
            } else {
                assert_eq!(
                    (fixture.pool.snapshots(), fixture.store.load().unwrap()),
                    before
                );
            }
        }
    }
}

#[test]
fn shared_reset_older_than_the_saved_refusal_never_clears_its_cooldown() {
    let fixture = Fixture::new();
    let saved = fixture.store.load().unwrap();
    let refusal = saved.profiles[0].quota_failure_at.unwrap();
    fixture
        .store
        .record_quota_reset(&fixture.id, refusal - chrono::Duration::nanoseconds(1))
        .unwrap();
    assert_eq!(fixture.store.load().unwrap(), saved);

    let future_failure = Utc::now() + chrono::Duration::days(30);
    let mut conflicted = saved;
    conflicted.profiles[0].quota_failure_at = Some(future_failure);
    conflicted.profiles[0].quota_reset_at = Some(future_failure - chrono::Duration::nanoseconds(1));
    conflicted.profiles[0].quota_reset_observed_at =
        Some(Utc::now() - chrono::Duration::minutes(1));
    fixture.store.save(&conflicted).unwrap();
    let before = fixture.pool.snapshots();
    fixture
        .store
        .apply_window_warmup_to_pool(&fixture.pool)
        .unwrap();
    assert_eq!(fixture.pool.snapshots(), before);
}

#[test]
fn shared_warmup_import_cannot_erase_a_pending_refusal_when_wall_time_moves_back() {
    let fixture = Fixture::new();
    let shared_reset = Utc::now() + chrono::Duration::minutes(1);
    let mut remote = fixture.store.load().unwrap();
    remote.profiles[0].quota_reset_at = Some(shared_reset);
    remote.profiles[0].quota_reset_observed_at =
        Some(Utc::now() - chrono::Duration::nanoseconds(1));
    remote.profiles[0].exhausted_until = None;
    remote.profiles[0].backend_resets_at = None;
    remote.profiles[0].rate_limits = AccountRateLimits::default();
    fixture.store.save(&remote).unwrap();
    fixture
        .pool
        .mark_exhausted(&fixture.failed, Some(fixture.reset))
        .unwrap();
    let before = fixture.pool.snapshots();
    fixture
        .store
        .apply_window_warmup_to_pool(&fixture.pool)
        .unwrap();
    assert_eq!(fixture.pool.snapshots(), before);
    fixture.store.synchronize(&fixture.pool).unwrap();
    let saved = fixture.store.load().unwrap();
    assert_eq!(saved.profiles[0].exhausted_until, Some(fixture.reset));
    assert!(saved.profiles[0].quota_failure_at > Some(shared_reset));
}

#[test]
fn explicit_maintenance_validates_ready_seat_without_inventing_recovery() {
    let fixture = Fixture::new();
    fixture.pool.reset_rate_limits(&fixture.id).unwrap();
    fixture.store.save_pool(&fixture.pool).unwrap();
    assert!(
        fixture
            .store
            .validate_profile_auth(&fixture.pool, &fixture.id, &fixture.auth)
            .unwrap()
    );
    assert!(
        fixture
            .store
            .capture_quota_probe(&fixture.pool, &fixture.id, &fixture.auth)
            .unwrap()
            .is_none()
    );
    let home = fixture.pool.snapshots()[0].profile.credential_home.clone();
    let mut stored = crate::load_auth_dot_json(
        &home,
        crate::AuthCredentialsStoreMode::File,
        crate::AuthKeyringBackendKind::default(),
    )
    .unwrap()
    .unwrap();
    stored.tokens.as_mut().unwrap().access_token = "synthetic-changed-before-post".into();
    crate::save_auth(
        &home,
        &stored,
        crate::AuthCredentialsStoreMode::File,
        crate::AuthKeyringBackendKind::default(),
    )
    .unwrap();
    assert!(
        !fixture
            .store
            .validate_profile_auth(&fixture.pool, &fixture.id, &fixture.auth)
            .unwrap()
    );
}
