use super::*;
use pretty_assertions::assert_eq;
use unicode_width::UnicodeWidthStr;

fn inventory() -> AccountInventory {
    let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    AccountInventory {
        generated_at: now.timestamp(),
        suspended: false,
        warnings: Vec::new(),
        settings: None,
        accounts: vec![AccountRow {
            profile_id: "acct-00000000000000000001-1".into(),
            label: Some("Work Pro".into()),
            active: true,
            priority: 10,
            state: AccountProfileState::Ready,
            disabled: false,
            login: LoginState::Cached,
            availability: "eligible".into(),
            plan: Some("Pro".into()),
            email: Some("alice@example.com".into()),
            cooldown_until: None,
            rate_limits: AccountRateLimits::default(),
            warmup: None,
        }],
    }
}

fn options(format: AccountOutputFormat) -> AccountOutputOptions {
    AccountOutputOptions {
        format,
        show_profile: false,
        details: false,
    }
}

#[test]
fn auto_uses_table_for_terminal_and_preserves_tsv_for_pipe() {
    assert_eq!(
        (
            AccountOutputFormat::Auto.resolve(OutputDestination::Terminal),
            AccountOutputFormat::Auto.resolve(OutputDestination::Pipe),
            AccountOutputFormat::Json.resolve(OutputDestination::Terminal),
        ),
        (
            AccountOutputFormat::Table,
            AccountOutputFormat::Tsv,
            AccountOutputFormat::Json
        )
    );
}

#[test]
fn default_tsv_keeps_existing_list_columns_and_unmasked_metadata() {
    insta::assert_snapshot!(
        render(&inventory(), AccountView::List, options(AccountOutputFormat::Tsv), /*columns*/ 80),
        @"
    ACTIVE	PRIORITY	STATE	PLAN	EMAIL	COOLDOWN	LABEL
    *	10	ready	Pro	alice@example.com	-	Work Pro
    "
    );
}

#[test]
fn table_sanitizes_controls_and_aligns_unicode_by_display_width() {
    let mut data = inventory();
    data.accounts[0].label = Some("研发\t\u{1b}[31m e\u{301}👩‍💻\u{202e}".into());
    let mut missing = data.accounts[0].clone();
    missing.profile_id = "acct-00000000000000000002-1".into();
    missing.label = Some("Personal".into());
    missing.login = LoginState::Missing;
    missing.availability = "login required".into();
    missing.active = false;
    data.accounts.push(missing);
    let output = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 120,
    );
    assert!(!output.contains(['\t', '\u{1b}', '\u{202e}']));
    let rows = output
        .lines()
        .filter(|line| line.contains("stored") || line.contains("missing"))
        .collect::<Vec<_>>();
    let header = output.lines().find(|line| line.contains("LOGIN")).unwrap();
    let login_column = UnicodeWidthStr::width(header.split("LOGIN").next().unwrap());
    assert_eq!(
        rows.iter()
            .map(|line| UnicodeWidthStr::width(
                line.split("stored")
                    .next()
                    .unwrap()
                    .split("missing")
                    .next()
                    .unwrap()
            ))
            .collect::<Vec<_>>(),
        vec![login_column; 2],
    );
    insta::assert_snapshot!(output);
}

#[test]
fn quota_samples_have_explicit_age_column_and_numeric_alignment() {
    let mut data = inventory();
    let now = data.now();
    data.accounts = [
        ("ceo", 32.0, 51.0, 0, 2),
        ("polaris949", 0.0, 31.0, 0, 0),
        ("admin", 95.0, 63.0, 25, 17),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (label, primary, secondary, primary_age, secondary_age))| {
            let mut row = data.accounts[0].clone();
            row.profile_id = format!("acct-0000000000000000000{index}-1");
            row.label = Some(label.into());
            row.priority = (index as u32 + 1) * 10;
            row.active = index == 0;
            row.plan = Some(if index == 1 { "Plus" } else { "Team" }.into());
            row.rate_limits = AccountRateLimits {
                primary: Some(codex_login::AccountRateLimitWindow {
                    used_percent: primary,
                    resets_at: Some(now + chrono::Duration::hours(3)),
                    window_minutes: Some(300),
                }),
                secondary: Some(codex_login::AccountRateLimitWindow {
                    used_percent: secondary,
                    resets_at: Some(now + chrono::Duration::days(2)),
                    window_minutes: Some(10080),
                }),
                observed_at: Some(now),
                window_observed_at: Some(codex_login::AccountWindowObservationTimes {
                    primary: Some(now - chrono::Duration::minutes(primary_age)),
                    secondary: Some(now - chrono::Duration::minutes(secondary_age)),
                }),
            };
            if index == 2 {
                row.availability = "cooldown".into();
                row.cooldown_until =
                    Some(now + chrono::Duration::hours(3) + chrono::Duration::minutes(29));
            }
            row
        },
    )
    .collect();
    let output = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 110,
    );
    assert!(output.contains("UPDATED"));
    assert!(output.contains("25m ago"));
    assert!(output.contains("retry 3h29m"));
    assert!(!output.contains("(25m)"));
    assert!(!output.contains("PROFILE"));
    assert_eq!(output.lines().count(), data.accounts.len() + 3);
    let header = output.lines().next().unwrap();
    let percent_columns = output
        .lines()
        .skip(1)
        .take(3)
        .map(|line| {
            line.match_indices('%')
                .map(|(index, _)| UnicodeWidthStr::width(&line[..index]))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(percent_columns, vec![percent_columns[0].clone(); 3]);
    assert!(header.contains("5H USED") && header.contains("WEEK USED"));
    insta::assert_snapshot!(output);
}

#[test]
fn elapsed_resets_and_mixed_durations_do_not_claim_current_five_hour_usage() {
    let mut data = inventory();
    let now = data.now();
    data.accounts[0].rate_limits = AccountRateLimits {
        primary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 95.0,
            resets_at: Some(now - chrono::Duration::minutes(1)),
            window_minutes: Some(60),
        }),
        secondary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 0.7,
            resets_at: Some(now + chrono::Duration::days(1)),
            window_minutes: Some(1440),
        }),
        observed_at: Some(now - chrono::Duration::minutes(25)),
        window_observed_at: None,
    };
    let mut other = data.accounts[0].clone();
    other.profile_id = "acct-00000000000000000002-1".into();
    other.label = Some("Personal".into());
    other.active = false;
    other.priority = 100;
    other.rate_limits.primary.as_mut().unwrap().window_minutes = Some(300);
    data.accounts.push(other);
    let output = render(
        &data,
        AccountView::Pool,
        options(AccountOutputFormat::Table),
        /*columns*/ 120,
    );
    assert!(output.contains("PRIMARY USED"));
    assert!(output.contains("1D USED"));
    assert!(output.contains("stale"));
    assert!(!output.contains("95%"));
    assert!(output.contains("<1%"));
    insta::assert_snapshot!(output);
}

#[test]
fn ambiguous_labels_keep_profile_ids_and_details_keep_original_stale_values() {
    let mut data = inventory();
    let now = data.now();
    let mut other = data.accounts[0].clone();
    other.profile_id = "acct-00000000000000000002-1".into();
    other.active = false;
    data.accounts.push(other);
    data.accounts[0].rate_limits = AccountRateLimits {
        primary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 99.8,
            resets_at: Some(now - chrono::Duration::minutes(1)),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now - chrono::Duration::minutes(20)),
        window_observed_at: None,
    };
    let output = render(
        &data,
        AccountView::List,
        AccountOutputOptions {
            details: true,
            ..options(AccountOutputFormat::Table)
        },
        /*columns*/ 140,
    );
    assert!(output.contains("PROFILE"));
    assert!(output.contains(&data.accounts[0].profile_id));
    assert!(output.contains(&data.accounts[1].profile_id));
    assert!(output.contains("Last sample: 99.8%"));
    assert!(output.contains("passed; awaiting refresh"));
    insta::assert_snapshot!(output);
}

#[test]
fn partial_sample_times_do_not_borrow_newer_watermark_and_future_usage_is_unknown() {
    let mut data = inventory();
    let now = data.now();
    data.accounts[0].rate_limits = AccountRateLimits {
        primary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 37.0,
            resets_at: Some(now + chrono::Duration::hours(2)),
            window_minutes: Some(300),
        }),
        secondary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 45.0,
            resets_at: Some(now + chrono::Duration::days(2)),
            window_minutes: Some(10080),
        }),
        observed_at: Some(now),
        window_observed_at: Some(codex_login::AccountWindowObservationTimes {
            primary: None,
            secondary: Some(now),
        }),
    };
    let unknown_time = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 120,
    );
    assert!(unknown_time.lines().nth(1).unwrap().ends_with("unknown"));
    data.accounts[0]
        .rate_limits
        .window_observed_at
        .as_mut()
        .unwrap()
        .primary = Some(now + chrono::Duration::minutes(1));
    let future = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 120,
    );
    assert!(!future.contains("37%"));
    assert!(future.contains("clock skew"));
    insta::assert_snapshot!(format!(
        "Unknown primary sample time:\n{unknown_time}\nFuture-dated primary sample:\n{future}"
    ));
}

#[test]
fn narrow_show_profile_wraps_full_selector_without_truncation() {
    let mut data = inventory();
    data.accounts[0].profile_id = "acct-1234567890abcdef1234567890abcdef-1".into();
    data.accounts[0].priority = u32::MAX;
    let output = render(
        &data,
        AccountView::List,
        AccountOutputOptions {
            show_profile: true,
            ..options(AccountOutputFormat::Table)
        },
        /*columns*/ 40,
    );
    assert!(output.contains(&data.accounts[0].profile_id));
    assert!(
        output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 40)
    );
    insta::assert_snapshot!(output);
}

#[test]
fn narrow_pool_cards_keep_login_and_quota_cooldown_separate() {
    let mut data = inventory();
    let now = data.now();
    data.accounts[0].availability = "cooldown".into();
    data.accounts[0].cooldown_until = Some(now + chrono::Duration::hours(2));
    data.accounts[0].rate_limits = AccountRateLimits {
        primary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 100.0,
            resets_at: Some(now + chrono::Duration::hours(2)),
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(now - chrono::Duration::minutes(5)),
        window_observed_at: None,
    };
    insta::assert_snapshot!(render(
        &data,
        AccountView::Pool,
        options(AccountOutputFormat::Table),
        /*columns*/ 40
    ));
}

#[test]
fn details_keep_full_email_and_unknown_independent_cache_ages() {
    let mut data = inventory();
    let now = data.now();
    let row = &mut data.accounts[0];
    row.label = Some("alice@example.com\nPrimary".into());
    row.rate_limits = AccountRateLimits {
        primary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 0.0,
            resets_at: Some(now + chrono::Duration::hours(5)),
            window_minutes: Some(300),
        }),
        secondary: Some(codex_login::AccountRateLimitWindow {
            used_percent: 9.0,
            resets_at: None,
            window_minutes: Some(10080),
        }),
        observed_at: Some(now + chrono::Duration::hours(1)),
        window_observed_at: Some(codex_login::AccountWindowObservationTimes {
            primary: Some(now - chrono::Duration::minutes(5)),
            secondary: Some(now + chrono::Duration::hours(1)),
        }),
    };
    let output = render(
        &data,
        AccountView::Pool,
        AccountOutputOptions {
            details: true,
            ..options(AccountOutputFormat::Table)
        },
        /*columns*/ 100,
    );
    assert!(output.contains("alice@example.com"));
    assert!(
        output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 100)
    );
    insta::assert_snapshot!(output);
}

#[test]
fn json_includes_all_profiles_and_only_nonsecret_metadata() {
    let mut data = inventory();
    data.suspended = true;
    data.accounts[0].active = false;
    data.accounts[0].disabled = true;
    data.accounts[0].availability = "disabled".into();
    let rendered = render(
        &data,
        AccountView::Pool,
        options(AccountOutputFormat::Json),
        /*columns*/ 80,
    );
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed, serde_json::to_value(data).unwrap());
    assert!(!rendered.contains("credential_home"));
}

#[test]
fn automatic_email_names_keep_raw_labels_in_machine_output() {
    let mut data = inventory();
    data.accounts[0].label = None;
    let mut custom = data.accounts[0].clone();
    custom.profile_id = "profile-2".into();
    custom.label = Some("Work".into());
    custom.active = false;
    let mut missing = data.accounts[0].clone();
    missing.profile_id = "profile-3".into();
    missing.email = None;
    missing.active = false;
    data.accounts.extend([custom, missing]);
    let json: serde_json::Value = serde_json::from_str(&render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Json),
        /*columns*/ 140,
    ))
    .unwrap();
    assert_eq!(json["accounts"][0]["label"], serde_json::Value::Null);
    let tsv = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Tsv),
        /*columns*/ 140,
    );
    assert!(tsv.lines().nth(1).unwrap().ends_with("\t-\t-"));
    let table = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 160,
    );
    assert!(table.contains("alice@example.com"));
    assert!(table.contains("Work"));
    assert!(table.contains("profile-3"));
    insta::assert_snapshot!(table);
}

#[test]
fn full_email_cards_preserve_long_identity_and_show_id_for_sanitized_names() {
    let mut data = inventory();
    data.accounts[0].label = None;
    data.accounts[0].email = Some("long.personal.address+business-seat@example.com".into());
    let mut other = data.accounts[0].clone();
    other.profile_id = "other-seat".into();
    other.label = Some("malicious\u{202e}name\n".into());
    other.active = false;
    data.accounts.push(other);
    let wide = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 180,
    );
    assert!(wide.contains("long.personal.address+business-seat@example.com"));
    let narrow = render(
        &data,
        AccountView::List,
        AccountOutputOptions {
            details: true,
            ..options(AccountOutputFormat::Table)
        },
        /*columns*/ 54,
    );
    assert!(narrow.contains("long.personal.address+business-seat@example.com"));
    assert!(!narrow.contains(['\u{202e}', '\u{1b}']));
    assert!(narrow.contains("other-seat"));
    insta::assert_snapshot!(format!("Wide:\n{wide}\nNarrow:\n{narrow}"));
}

#[test]
fn an_email_matching_another_accounts_label_keeps_profile_selectors_visible() {
    let mut data = inventory();
    data.accounts[0].label = Some("bob@example.com".into());
    let mut other = data.accounts[0].clone();
    other.profile_id = "other-seat".into();
    other.label = Some("Business".into());
    other.email = Some("bob@example.com".into());
    other.active = false;
    data.accounts.push(other);
    let output = render(
        &data,
        AccountView::List,
        options(AccountOutputFormat::Table),
        /*columns*/ 150,
    );
    assert!(output.contains("PROFILE"));
    insta::assert_snapshot!(output);
}
