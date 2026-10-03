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
        .filter(|line| line.contains("acct-"))
        .collect::<Vec<_>>();
    let header = output.lines().find(|line| line.contains("LOGIN")).unwrap();
    let login_column = UnicodeWidthStr::width(header.split("LOGIN").next().unwrap());
    assert_eq!(
        rows.iter()
            .map(|line| UnicodeWidthStr::width(
                line.split("cached")
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
fn details_mask_identity_and_keep_unknown_and_independent_cache_ages() {
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
    assert!(!output.contains("alice@example.com"));
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
