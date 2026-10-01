use super::*;
use codex_app_server_protocol::AccountPoolRateLimits;
use codex_login::format_reset_countdown;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn account_picker_starts_on_active_account_and_skips_disabled_profile() {
    let (mut chat, _tx, mut rx, _op_rx) =
        crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    let active = AccountPoolAccount {
        profile_id: "work".into(),
        label: Some("Work seat".into()),
        priority: 0,
        is_active: true,
        availability: AccountPoolAvailability::Available,
        plan_type: None,
        email: None,
        rate_limits: AccountPoolRateLimits::default(),
        window_warmup: None,
    };
    let pool = AccountPoolReadResponse {
        enabled: true,
        active_profile_id: Some(active.profile_id.clone()),
        active_generation: Some(1),
        accounts: vec![
            AccountPoolAccount {
                profile_id: "personal".into(),
                label: Some("Personal account".into()),
                priority: 10,
                is_active: false,
                ..active.clone()
            },
            active.clone(),
            AccountPoolAccount {
                profile_id: "paused".into(),
                label: Some("Paused account".into()),
                is_active: false,
                availability: AccountPoolAvailability::Disabled,
                ..active.clone()
            },
            AccountPoolAccount {
                profile_id: "relogin".into(),
                label: Some("Sign-in expired".into()),
                is_active: false,
                availability: AccountPoolAvailability::AuthenticationUnavailable {
                    reason: "Refresh token expired".into(),
                },
                ..active
            },
        ],
    };
    chat.open_account_pool_picker(Ok(pool.clone()));
    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match rx.try_recv().expect("account selection event") {
        AppEvent::ActivateAccountPoolProfile { profile_id, force } => {
            assert_eq!((profile_id.as_deref(), force), (Some("work"), false));
        }
        other => panic!("expected account activation, got {other:?}"),
    }
    chat.open_account_pool_picker(Ok(pool));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match rx.try_recv().expect("automatic selection event") {
        AppEvent::ActivateAccountPoolProfile { profile_id, force } => {
            assert_eq!((profile_id, force), (None, false));
        }
        other => panic!("expected automatic selection, got {other:?}"),
    }
}

#[tokio::test]
async fn exhausted_profile_requires_the_labeled_retry_action() {
    let (mut chat, _tx, mut rx, _op_rx) =
        crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    chat.open_account_pool_picker(Ok(AccountPoolReadResponse {
        enabled: true,
        active_profile_id: None,
        active_generation: None,
        accounts: vec![AccountPoolAccount {
            profile_id: "work".into(),
            label: Some("Work seat".into()),
            priority: 0,
            is_active: false,
            availability: AccountPoolAvailability::Exhausted { resets_at: None },
            plan_type: None,
            email: None,
            rate_limits: AccountPoolRateLimits::default(),
            window_warmup: None,
        }],
    }));
    let rendered = crate::chatwidget::tests::render_bottom_popup(&chat, /*width*/ 80);
    assert!(rendered.contains("Retry Work seat"), "{rendered}");
    assert!(rendered.contains("Clear the local cooldown"), "{rendered}");
    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match rx.try_recv().expect("explicit retry event") {
        AppEvent::ActivateAccountPoolProfile { profile_id, force } => {
            assert_eq!((profile_id.as_deref(), force), (Some("work"), true));
        }
        other => panic!("expected account retry, got {other:?}"),
    }
}

#[tokio::test]
async fn pool_quota_observations_keep_identity_and_only_activation_requires_refresh() {
    let (mut chat, _tx, _rx, _op_rx) =
        crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
    let mut update = AccountPoolUpdatedNotification {
        active_profile_id: Some("work".into()),
        active_generation: Some(1),
        accounts: vec![AccountPoolAccount {
            profile_id: "work".into(),
            label: Some("Work".into()),
            priority: 0,
            is_active: true,
            availability: AccountPoolAvailability::Available,
            plan_type: None,
            email: Some("member@example.com".into()),
            rate_limits: AccountPoolRateLimits::default(),
            window_warmup: None,
        }],
    };
    assert!(chat.on_account_pool_updated(&update));
    update.accounts[0].rate_limits.primary = Some(AccountPoolRateLimitWindow {
        used_percent: 60.0,
        resets_at: Some(1_900_000_000),
    });
    assert!(!chat.on_account_pool_updated(&update));
    update.accounts[0].label = Some("Work seat".into());
    assert!(!chat.on_account_pool_updated(&update));
    update.active_generation = Some(2);
    assert!(chat.on_account_pool_updated(&update));
}

#[test]
fn reset_countdown_keeps_hours_compact_and_labels_days() {
    assert_eq!(format_reset_countdown(/*remaining_seconds*/ 1), "0:01");
    assert_eq!(format_reset_countdown(/*remaining_seconds*/ 60), "0:01");
    assert_eq!(
        format_reset_countdown(/*remaining_seconds*/ 3 * 60 * 60 + 46 * 60),
        "3:46"
    );
    assert_eq!(
        format_reset_countdown(
            /*remaining_seconds*/ 3 * 24 * 60 * 60 + 21 * 60 * 60 + 2 * 60
        ),
        "3d 21h 2m"
    );
}

#[test]
fn rate_limit_descriptions_color_remaining_percent_and_countdown_by_window() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let window = AccountPoolRateLimitWindow {
        used_percent: 38.0,
        resets_at: Some(now.timestamp() + 3 * 60 * 60 + 46 * 60),
    };

    assert_eq!(
        account_rate_limit_description(&window, AccountRateLimitKind::FiveHour, now),
        vec![
            "62%".cyan(),
            " 5h left".dim(),
            ", reset in ".dim(),
            "3:46".cyan(),
        ]
    );
    assert_eq!(
        account_rate_limit_description(&window, AccountRateLimitKind::Weekly, now),
        vec![
            "62%".magenta(),
            " weekly left".dim(),
            ", reset in ".dim(),
            "3:46".magenta(),
        ]
    );
}

#[test]
fn zero_percent_five_hour_window_does_not_claim_whether_it_started() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let idle = AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(now.timestamp() + 5 * 60 * 60),
    };

    assert_eq!(
        account_rate_limit_description(&idle, AccountRateLimitKind::FiveHour, now),
        vec![
            "100%".cyan(),
            " 5h left".dim(),
            ", ".dim(),
            "start unconfirmed".cyan(),
        ]
    );
    // Weekly windows can legitimately sit at 0% with a real countdown.
    assert_eq!(
        account_rate_limit_description(&idle, AccountRateLimitKind::Weekly, now),
        vec![
            "100%".magenta(),
            " weekly left".dim(),
            ", reset in ".dim(),
            "5:00".magenta(),
        ]
    );
}

#[test]
fn elapsed_and_unknown_resets_have_compact_output() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let elapsed = AccountPoolRateLimitWindow {
        used_percent: 40.0,
        resets_at: Some(now.timestamp() - 1),
    };
    let unknown = AccountPoolRateLimitWindow {
        used_percent: 40.0,
        resets_at: None,
    };

    assert_eq!(
        account_rate_limit_description(&elapsed, AccountRateLimitKind::Weekly, now),
        vec![
            "60%".magenta(),
            " weekly left".dim(),
            ", reset ".dim(),
            "now".magenta(),
        ]
    );
    assert_eq!(
        account_rate_limit_description(&unknown, AccountRateLimitKind::Weekly, now),
        vec!["60%".magenta(), " weekly left".dim()]
    );
}

#[test]
fn account_display_name_prefers_label_over_email_and_profile_id() {
    let with_email = AccountPoolAccount {
        profile_id: "primary-acct".to_string(),
        label: Some("Team plan".to_string()),
        priority: 0,
        is_active: true,
        availability: AccountPoolAvailability::Available,
        plan_type: None,
        email: Some("primary@example.com".to_string()),
        rate_limits: AccountPoolRateLimits::default(),
        window_warmup: None,
    };
    assert_eq!(account_display_name(&with_email), "Team plan".to_string());

    let label_only = AccountPoolAccount {
        email: None,
        ..with_email.clone()
    };
    assert_eq!(account_display_name(&label_only), "Team plan".to_string());

    let email_only = AccountPoolAccount {
        label: Some(" ".to_string()),
        ..with_email.clone()
    };
    assert_eq!(account_display_name(&email_only), "primary@example.com");

    let id_only = AccountPoolAccount {
        label: None,
        email: None,
        ..with_email
    };
    assert_eq!(account_display_name(&id_only), "primary-acct".to_string());
}

#[test]
fn idle_warmup_description_distinguishes_unconfirmed_completion_from_failure() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let account = AccountPoolAccount {
        profile_id: "backup".to_string(),
        label: None,
        priority: 10,
        is_active: false,
        availability: AccountPoolAvailability::Available,
        plan_type: None,
        email: None,
        rate_limits: AccountPoolRateLimits {
            primary: Some(AccountPoolRateLimitWindow {
                used_percent: 0.0,
                resets_at: Some(now.timestamp() + 5 * 3600),
            }),
            secondary: None,
            observed_at: None,
        },
        window_warmup: Some(AccountPoolWindowWarmup {
            outcome: AccountPoolWindowWarmupOutcome::Succeeded,
            phase: None,
            consecutive_failures: None,
            attempted_at: now.timestamp(),
            retry_after: Some(now.timestamp() + 300),
        }),
    };
    let description = account_description(&account, now);
    let succeeded = description
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    let mut failed = account;
    failed.window_warmup = Some(AccountPoolWindowWarmup {
        outcome: AccountPoolWindowWarmupOutcome::Failed,
        phase: None,
        consecutive_failures: None,
        attempted_at: now.timestamp(),
        retry_after: Some(now.timestamp() + 60),
    });
    let description = account_description(&failed, now);
    let failure = description
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    failed.window_warmup.as_mut().unwrap().consecutive_failures = Some(0);
    let deferred = account_description(&failed, now)
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    failed.window_warmup.as_mut().unwrap().phase = Some(AccountPoolWindowWarmupPhase::InProgress);
    let pending = account_description(&failed, now)
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    failed.window_warmup.as_mut().unwrap().phase = Some(AccountPoolWindowWarmupPhase::Unconfirmed);
    let unconfirmed = account_description(&failed, now)
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    insta::assert_snapshot!(format!(
        "Completed, quota still zero:\n{succeeded}\n\nFailed:\n{failure}\n\nDeferred, no counted failure:\n{deferred}\n\nIn progress:\n{pending}\n\nCompleted, awaiting quota evidence:\n{unconfirmed}"
    ));
    assert!(succeeded.contains("warmup done; start unconfirmed"));
    assert!(failure.contains("warmup failed; retry in 0:01"));
    assert!(deferred.contains("warmup deferred; check in 0:01"));
    assert!(pending.contains("warmup in progress"));
    assert!(unconfirmed.contains("warmup sent; start unconfirmed"));
}

#[test]
fn started_five_hour_window_hides_warmup_failure() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let account = AccountPoolAccount {
        profile_id: "backup".to_string(),
        label: None,
        priority: 10,
        is_active: false,
        availability: AccountPoolAvailability::Available,
        plan_type: None,
        email: None,
        rate_limits: AccountPoolRateLimits {
            primary: Some(AccountPoolRateLimitWindow {
                used_percent: 4.0,
                resets_at: Some(now.timestamp() + 5 * 3600),
            }),
            secondary: None,
            observed_at: None,
        },
        window_warmup: Some(AccountPoolWindowWarmup {
            outcome: AccountPoolWindowWarmupOutcome::Failed,
            phase: None,
            consecutive_failures: None,
            attempted_at: now.timestamp(),
            retry_after: Some(now.timestamp() + 60),
        }),
    };
    let description = account_description(&account, now);
    assert!(
        description
            .iter()
            .all(|span| !span.content.contains("warmup failed")),
        "{description:?}"
    );
}

#[test]
fn active_account_hides_leftover_warmup_failure() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let account = AccountPoolAccount {
        profile_id: "backup".to_string(),
        label: None,
        priority: 10,
        is_active: true,
        availability: AccountPoolAvailability::Available,
        plan_type: None,
        email: None,
        rate_limits: AccountPoolRateLimits {
            primary: Some(AccountPoolRateLimitWindow {
                used_percent: 0.0,
                resets_at: Some(now.timestamp() + 5 * 3600),
            }),
            secondary: None,
            observed_at: None,
        },
        window_warmup: Some(AccountPoolWindowWarmup {
            outcome: AccountPoolWindowWarmupOutcome::Failed,
            phase: None,
            consecutive_failures: None,
            attempted_at: now.timestamp(),
            retry_after: Some(now.timestamp() + 60),
        }),
    };
    let description = account_description(&account, now);
    assert!(
        description
            .iter()
            .all(|span| !span.content.contains("warmup failed")),
        "{description:?}"
    );
}
