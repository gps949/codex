use super::*;
use codex_app_server_protocol::AccountPoolRateLimits;
use codex_login::format_reset_countdown;
use pretty_assertions::assert_eq;

#[test]
fn reset_countdown_uses_compact_colon_units() {
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
        "3:21:02"
    );
}

#[test]
fn rate_limit_descriptions_color_percent_and_countdown_by_window() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let window = AccountPoolRateLimitWindow {
        used_percent: 38.0,
        resets_at: Some(now.timestamp() + 3 * 60 * 60 + 46 * 60),
    };

    assert_eq!(
        account_rate_limit_description(&window, AccountRateLimitKind::FiveHour, now),
        vec![
            "38%".cyan(),
            " 5h used".dim(),
            ", reset in ".dim(),
            "3:46".cyan(),
        ]
    );
    assert_eq!(
        account_rate_limit_description(&window, AccountRateLimitKind::Weekly, now),
        vec![
            "38%".magenta(),
            " weekly used".dim(),
            ", reset in ".dim(),
            "3:46".magenta(),
        ]
    );
}

#[test]
fn zero_percent_five_hour_window_marks_not_started() {
    let now = DateTime::from_timestamp(/*secs*/ 1_800_000_000, /*nsecs*/ 0).unwrap();
    let idle = AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(now.timestamp() + 5 * 60 * 60),
    };

    assert_eq!(
        account_rate_limit_description(&idle, AccountRateLimitKind::FiveHour, now),
        vec![
            "0%".cyan(),
            " 5h used".dim(),
            ", ".dim(),
            "not started".cyan(),
        ]
    );
    // Weekly windows can legitimately sit at 0% with a real countdown.
    assert_eq!(
        account_rate_limit_description(&idle, AccountRateLimitKind::Weekly, now),
        vec![
            "0%".magenta(),
            " weekly used".dim(),
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
            "40%".magenta(),
            " weekly used".dim(),
            ", reset ".dim(),
            "now".magenta(),
        ]
    );
    assert_eq!(
        account_rate_limit_description(&unknown, AccountRateLimitKind::Weekly, now),
        vec!["40%".magenta(), " weekly used".dim()]
    );
}

#[test]
fn account_display_name_prefers_email_over_label_and_profile_id() {
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
    assert_eq!(
        account_display_name(&with_email),
        "primary@example.com".to_string()
    );

    let label_only = AccountPoolAccount {
        email: None,
        ..with_email.clone()
    };
    assert_eq!(account_display_name(&label_only), "Team plan".to_string());

    let id_only = AccountPoolAccount {
        label: None,
        email: None,
        ..with_email
    };
    assert_eq!(account_display_name(&id_only), "primary-acct".to_string());
}

#[test]
fn standby_warmup_status_appears_in_account_description() {
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
            attempted_at: now.timestamp(),
            retry_after: Some(now.timestamp() + 300),
        }),
    };
    let description = account_description(&account, now);
    assert!(
        description
            .iter()
            .any(|span| span.content.contains("5h warmed")),
        "{description:?}"
    );
}
