use chrono::Duration;
use codex_config::AccountPoolRotationStrategy;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

use super::*;
use crate::AccountAvailabilityMutation;
use crate::AccountPool;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AuthManager;
use crate::CodexAuth;

fn scheduling_pool(
    strategy: AccountPoolRotationStrategy,
) -> (AccountPool, AccountProfileId, AccountProfileId) {
    let pool = AccountPool::new();
    pool.set_rotation_strategy(strategy);
    let [first, second] =
        ["first", "second"].map(|name| AccountProfileId::new(name).expect("valid fixture profile"));
    for (id, priority) in [(&first, 0), (&second, 10)] {
        pool.register(
            AccountProfile::new(
                id.clone(),
                PathBuf::from(id.as_str()),
                priority,
                /*label*/ None,
            ),
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
        )
        .expect("register fixture profile");
    }
    (pool, first, second)
}

#[test]
fn automatic_selection_prefers_headroom_and_retains_a_depleted_last_probe() {
    for strategy in [
        AccountPoolRotationStrategy::FillFirst,
        AccountPoolRotationStrategy::EarliestReset,
    ] {
        let now = Utc::now();
        let exhausted = AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        };
        for (primary, secondary) in [
            (Some(exhausted.clone()), None),
            (None, Some(exhausted.clone())),
        ] {
            let (pool, first, second) = scheduling_pool(strategy);
            pool.update_rate_limits(
                &first,
                AccountRateLimits {
                    primary,
                    secondary,
                    observed_at: Some(now),
                },
            )
            .expect("record fresh exhausted quota");
            pool.update_rate_limits(
                &second,
                AccountRateLimits {
                    primary: Some(AccountRateLimitWindow {
                        used_percent: 70.0,
                        resets_at: Some(now + Duration::hours(2)),
                        window_minutes: Some(300),
                    }),
                    observed_at: Some(now),
                    ..AccountRateLimits::default()
                },
            )
            .expect("record usable backup quota");
            let lease = pool.activate_fill_first().expect("select usable headroom");
            assert_eq!(lease.profile().id, second);
            let AccountAvailabilityMutation::Rebound(last_probe) = pool
                .mark_exhausted(&lease, Some(now + Duration::hours(2)))
                .expect("mark backup exhausted")
            else {
                panic!("cached quota must not remove the last schedulable probe");
            };
            assert_eq!(last_probe.profile().id, first);
        }
    }
}

#[test]
fn automatic_selection_probes_stale_unknown_and_reset_quota() {
    for strategy in [
        AccountPoolRotationStrategy::FillFirst,
        AccountPoolRotationStrategy::EarliestReset,
    ] {
        let now = Utc::now();
        for (observed_at, resets_at) in [
            (
                Some(now - Duration::minutes(31)),
                Some(now + Duration::hours(1)),
            ),
            (None, Some(now + Duration::hours(1))),
            (Some(now), Some(now - Duration::seconds(1))),
        ] {
            let (pool, first, second) = scheduling_pool(strategy);
            pool.update_rate_limits(
                &first,
                AccountRateLimits {
                    primary: Some(AccountRateLimitWindow {
                        used_percent: 100.0,
                        resets_at,
                        window_minutes: Some(300),
                    }),
                    observed_at,
                    ..AccountRateLimits::default()
                },
            )
            .expect("record advisory quota");
            pool.update_rate_limits(
                &second,
                AccountRateLimits {
                    primary: Some(AccountRateLimitWindow {
                        used_percent: 70.0,
                        resets_at: Some(now + Duration::hours(2)),
                        window_minutes: Some(300),
                    }),
                    observed_at: Some(now),
                    ..AccountRateLimits::default()
                },
            )
            .expect("record backup quota");
            assert_eq!(
                pool.activate_fill_first()
                    .expect("probe advisory quota")
                    .profile()
                    .id,
                first,
            );
        }
    }
}

#[test]
fn preemptive_rotation_keeps_headroom_when_the_backup_is_known_depleted() {
    for strategy in [
        AccountPoolRotationStrategy::FillFirst,
        AccountPoolRotationStrategy::EarliestReset,
    ] {
        let (pool, first, second) = scheduling_pool(strategy);
        let lease = pool.lease().expect("initial lease");
        let now = Utc::now();
        pool.update_rate_limits(
            &second,
            AccountRateLimits {
                primary: Some(AccountRateLimitWindow {
                    used_percent: 100.0,
                    resets_at: Some(now + Duration::hours(1)),
                    window_minutes: Some(300),
                }),
                observed_at: Some(now),
                ..AccountRateLimits::default()
            },
        )
        .expect("record depleted backup");
        assert!(
            pool.rotate_preemptively(&lease, now + Duration::hours(1))
                .is_none()
        );
        assert_eq!(
            pool.lease().expect("retain working lease").profile().id,
            first
        );
    }
}

#[test]
fn automatic_selection_reclaims_parked_headroom_before_known_depleted_quota() {
    for strategy in [
        AccountPoolRotationStrategy::FillFirst,
        AccountPoolRotationStrategy::EarliestReset,
    ] {
        let (pool, first, second) = scheduling_pool(strategy);
        let lease = pool.lease().expect("initial lease");
        let now = Utc::now();
        pool.rotate_preemptively(&lease, now + Duration::hours(1))
            .expect("park the first profile before backup depletion");
        pool.update_rate_limits(
            &second,
            AccountRateLimits {
                primary: Some(AccountRateLimitWindow {
                    used_percent: 100.0,
                    resets_at: Some(now + Duration::hours(2)),
                    window_minutes: Some(300),
                }),
                observed_at: Some(now),
                ..AccountRateLimits::default()
            },
        )
        .expect("observe depleted backup without a failed request");
        assert_eq!(
            pool.activate_fill_first()
                .expect("recover parked headroom")
                .profile()
                .id,
            first,
        );
    }
}

#[test]
fn late_same_window_quota_cannot_replace_usage_or_omit_weekly_quota() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        secondary: Some(AccountRateLimitWindow {
            used_percent: 65.0,
            resets_at: Some(now + Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 10.0,
            resets_at: existing
                .primary
                .as_ref()
                .and_then(|window| window.resets_at),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now + Duration::seconds(1)),
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming),
        AccountRateLimits {
            observed_at: Some(now + Duration::seconds(1)),
            ..existing.clone()
        }
    );
}

#[test]
fn quota_decrease_is_allowed_when_backend_reports_a_new_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now + Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming.clone()),
        incoming
    );
}

#[test]
fn old_probe_cannot_overwrite_a_more_recent_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 99.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now - Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(merge_rate_limits_monotonic(&existing, incoming), existing);
}

#[test]
fn late_observation_of_an_older_window_preserves_the_current_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 75.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now + Duration::seconds(1)),
        ..AccountRateLimits::default()
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming),
        AccountRateLimits {
            observed_at: Some(now + Duration::seconds(1)),
            ..existing
        }
    );
}
