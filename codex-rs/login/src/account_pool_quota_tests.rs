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

                    window_observed_at: None,
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

        window_observed_at: None,
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

        window_observed_at: None,
    };
    assert_eq!(
        merge_rate_limits_monotonic(&existing, incoming),
        AccountRateLimits {
            observed_at: Some(now + Duration::seconds(1)),
            window_observed_at: Some(AccountWindowObservationTimes {
                primary: Some(now + Duration::seconds(1)),
                secondary: Some(now),
            }),
            ..existing.clone()
        }
    );
}

#[test]
fn cached_exhaustion_does_not_become_fresh_after_other_window_observation() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now - Duration::hours(2)),
        ..AccountRateLimits::default()
    };
    let incoming = AccountRateLimits {
        secondary: Some(AccountRateLimitWindow {
            used_percent: 20.0,
            resets_at: Some(now + Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let merged = merge_rate_limits_monotonic(&existing, incoming);
    assert!(!has_fresh_exhausted_window(&merged, &now));
}

#[test]
fn later_primary_observation_does_not_discard_a_newer_secondary_window() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        secondary: Some(AccountRateLimitWindow {
            used_percent: 70.0,
            resets_at: Some(now + Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now - Duration::hours(2)),
        ..AccountRateLimits::default()
    };
    let primary = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 30.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    let primary = merge_rate_limits_monotonic(&existing, primary);
    let secondary = AccountRateLimits {
        secondary: Some(AccountRateLimitWindow {
            used_percent: 85.0,
            ..existing.secondary.unwrap()
        }),
        observed_at: Some(now - Duration::minutes(1)),
        ..AccountRateLimits::default()
    };
    let merged = merge_rate_limits_monotonic(&primary, secondary);
    let mut expected = primary;
    expected.secondary.as_mut().unwrap().used_percent = 85.0;
    assert_eq!(merged.secondary, expected.secondary);
}

#[test]
fn reset_epoch_rejects_old_windows_inside_newer_partial_snapshots() {
    let now = Utc::now();
    let (pool, first, _) = scheduling_pool(AccountPoolRotationStrategy::FillFirst);
    pool.apply_quota_reset(&first, now - Duration::seconds(10))
        .unwrap();
    let mixed = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + Duration::hours(1)),
            window_minutes: Some(300),
        }),
        secondary: Some(AccountRateLimitWindow {
            used_percent: 20.0,
            resets_at: Some(now + Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),
        window_observed_at: Some(AccountWindowObservationTimes {
            primary: Some(now - Duration::minutes(1)),
            secondary: Some(now),
        }),
    };
    pool.update_rate_limits(&first, mixed.clone()).unwrap();
    let actual = pool
        .snapshots()
        .into_iter()
        .find(|account| account.profile.id == first)
        .unwrap()
        .rate_limits;
    assert_eq!((actual.primary, actual.secondary), (None, mixed.secondary));
}

#[test]
fn empty_quota_reply_does_not_refresh_or_change_an_existing_observation() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 10.0,
            resets_at: None,
            window_minutes: Some(300),
        }),
        observed_at: Some(now - Duration::hours(1)),
        ..AccountRateLimits::default()
    };
    let empty = AccountRateLimits {
        observed_at: Some(now),
        ..AccountRateLimits::default()
    };
    assert_eq!(merge_rate_limits_monotonic(&existing, empty), existing);
}

#[test]
fn quota_failure_preserves_known_windows_when_error_metadata_is_partial() {
    let now = Utc::now();
    for primary_used in [None, Some(100.0)] {
        let (pool, first, _) = scheduling_pool(AccountPoolRotationStrategy::FillFirst);
        let lease = pool.activate(&first).expect("activate first profile");
        let existing = AccountRateLimits {
            primary: Some(AccountRateLimitWindow {
                used_percent: 83.0,
                resets_at: Some(now + Duration::hours(1)),
                window_minutes: Some(300),
            }),
            secondary: Some(AccountRateLimitWindow {
                used_percent: 98.0,
                resets_at: Some(now + Duration::days(2)),
                window_minutes: Some(10080),
            }),
            observed_at: Some(now - Duration::hours(2)),

            window_observed_at: None,
        };
        pool.update_rate_limits(&first, existing.clone())
            .expect("cache both windows");
        let mut incoming = AccountRateLimits {
            observed_at: Some(now),
            ..AccountRateLimits::default()
        };
        if let Some(used_percent) = primary_used {
            incoming.primary = Some(AccountRateLimitWindow {
                used_percent,
                ..existing.primary.clone().unwrap()
            });
        }
        let expected = AccountRateLimits {
            primary: Some(AccountRateLimitWindow {
                used_percent: primary_used.unwrap_or(83.0),
                ..existing.primary.clone().unwrap()
            }),
            secondary: existing.secondary,
            observed_at: if primary_used.is_some() {
                Some(now)
            } else {
                existing.observed_at
            },

            window_observed_at: primary_used.map(|_| AccountWindowObservationTimes {
                primary: Some(now),
                secondary: existing.observed_at,
            }),
        };
        pool.mark_exhausted_with_rate_limits(&lease, Some(now + Duration::hours(1)), incoming)
            .expect("process quota rejection");
        let actual = pool
            .snapshots()
            .into_iter()
            .find(|account| account.profile.id == first)
            .unwrap();
        assert_eq!(
            (actual.rate_limits, actual.availability),
            (
                expected,
                crate::AccountAvailability::Exhausted {
                    resets_at: Some(now + Duration::hours(1)),
                }
            )
        );
        if primary_used.is_some() {
            let rejected = pool
                .snapshots()
                .into_iter()
                .find(|account| account.profile.id == first)
                .unwrap();
            pool.update_rate_limits(
                &first,
                AccountRateLimits {
                    primary: Some(AccountRateLimitWindow {
                        used_percent: 1.0,
                        resets_at: Some(now + Duration::hours(3)),
                        window_minutes: Some(300),
                    }),
                    secondary: None,
                    observed_at: Some(now - Duration::minutes(1)),

                    window_observed_at: None,
                },
            )
            .expect("process a delayed pre-rejection observation");
            let after = pool
                .snapshots()
                .into_iter()
                .find(|account| account.profile.id == first)
                .unwrap();
            assert_eq!(after.rate_limits, rejected.rate_limits);
        }
    }
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
    assert_eq!(merge_rate_limits_monotonic(&existing, incoming), existing);
}

#[test]
fn fresh_timed_observation_replaces_stale_untimed_usage_and_window_shape() {
    let now = Utc::now();
    let existing = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 3.0,
            resets_at: None,
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now - Duration::minutes(31)),

        window_observed_at: None,
    };
    let idle = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now),

        window_observed_at: None,
    };
    assert_eq!(merge_rate_limits_monotonic(&existing, idle.clone()), idle);
    let changed = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(60),
        }),
        observed_at: Some(now + Duration::seconds(1)),
        ..idle.clone()
    };
    assert_eq!(merge_rate_limits_monotonic(&idle, changed.clone()), changed);
}

#[test]
fn positive_primary_usage_can_confirm_a_tentative_idle_reset_without_losing_weekly_usage() {
    let now = Utc::now();
    let idle = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + Duration::hours(5)),
            window_minutes: Some(300),
        }),
        secondary: Some(AccountRateLimitWindow {
            used_percent: 8.0,
            resets_at: Some(now + Duration::days(4)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now - Duration::seconds(2)),

        window_observed_at: None,
    };
    let confirmed = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 1.0,
            resets_at: Some(now + Duration::hours(5) - Duration::seconds(1)),
            window_minutes: Some(300),
        }),
        secondary: idle.secondary.clone(),
        observed_at: Some(now),

        window_observed_at: None,
    };
    assert_eq!(
        merge_rate_limits_monotonic(&idle, confirmed.clone()),
        confirmed
    );
}
