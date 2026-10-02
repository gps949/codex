use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn pool() -> AccountPoolReadResponse {
    serde_json::from_value(json!({
        "enabled": true, "activeProfileId": "secret-work-id", "activeGeneration": 1,
        "accounts": [{"profileId":"secret-work-id", "label":"Work", "priority":10,
        "isActive":true, "availability":{"type":"available"}, "planType":null, "email":null,
        "rateLimits":{"primary":{"usedPercent":37.0,"resetsAt":1900000000},"secondary":null,"observedAt":null},
        "windowWarmup":null}]
    })).unwrap()
}

#[test]
fn mobile_account_compact_views_show_selectors_and_unknown_quota() {
    let mut pool = pool();
    pool.accounts[0]
        .rate_limits
        .primary
        .as_mut()
        .unwrap()
        .resets_at = None;
    insta::assert_snapshot!(list(&pool, 1).unwrap(), @"
    Accounts · 1/1
    Cached quota

    Work · Current
    Select: /account use @secret-w
    Primary: 37% used · age unknown
    Reset: unknown
    Secondary: unknown used · age unknown
    Reset: unknown

    Each window retains its last accepted observation.
    Details: /account show <label|@selector>
    Controls: /account help
    ");
    assert_eq!(pool_caption(&pool).as_deref(), Some("Work · 1/1 ready"));
    assert_eq!(resolve(&pool, "\"Work\"").unwrap(), &pool.accounts[0]);
}

#[test]
fn mobile_account_label_prefers_custom_label_over_email() {
    let mut pool = pool();
    pool.accounts[0].email = Some("work@example.com".into());
    pool.accounts[0].label = Some("Work".into());
    assert_eq!(label(&pool.accounts[0]), "Work");
    assert_eq!(pool_caption(&pool).as_deref(), Some("Work · 1/1 ready"));
    assert_eq!(
        resolve(&pool, "work@example.com").unwrap(),
        &pool.accounts[0]
    );
}

#[test]
fn mobile_account_list_shows_reset_times_and_cooldown_retry() {
    let mut pool = pool();
    pool.accounts[0].availability = AccountPoolAvailability::Exhausted { resets_at: None };
    pool.accounts[0]
        .rate_limits
        .primary
        .as_mut()
        .unwrap()
        .resets_at = Some(0);
    let text = list(&pool, 1).unwrap();
    assert!(text.contains("Reset: passed · awaiting refresh"));
    assert!(text.contains("Retry: /account retry @secret-w"));
    assert!(text.contains("Primary: 37% used · age unknown"));
    insta::assert_snapshot!("mobile_account_list_with_reset_and_retry", text);
}

#[test]
fn mobile_account_same_email_profiles_have_stable_selectors() {
    let mut pool = pool();
    pool.accounts[0].email = Some("member@example.com".into());
    let mut personal = pool.accounts[0].clone();
    personal.profile_id = "personal-profile".into();
    personal.label = Some("Personal".into());
    personal.is_active = false;
    pool.accounts.push(personal);
    assert_eq!(resolve(&pool, "@personal").unwrap(), &pool.accounts[1]);
    assert_eq!(resolve(&pool, "Personal").unwrap(), &pool.accounts[1]);
    assert!(resolve(&pool, "member@example.com").is_err());
    let text = list(&pool, 1).unwrap();
    assert!(text.contains("Personal"));
    assert!(text.contains("/account use @personal"));
    let text = detail(&pool, "@personal").unwrap();
    assert!(text.contains("Email: member@example.com"));
    assert!(text.contains("Select: /account use @personal"));
    pool.accounts[0].profile_id = "personal-work-profile".into();
    assert!(
        resolve(&pool, "@personal")
            .unwrap_err()
            .contains("Ambiguous")
    );
}

#[test]
fn mobile_account_pages_are_bounded_and_names_cannot_inject_markup() {
    let mut pool = pool();
    let mut other = pool.accounts[0].clone();
    other.is_active = false;
    other.label = Some("[bad](https://example.com)\n**name**".repeat(100));
    pool.accounts.extend(std::iter::repeat_n(other, 8));
    let text = list(&pool, 1).unwrap();
    assert_eq!(text.matches("Primary:").count(), 4);
    assert!(text.contains("Next: /account list 2"));
    assert_eq!(text.matches("Select: /account use @").count(), 4);
    assert!(!text.contains("[bad]"));
    assert!(text.len() < 1200);
    assert!(list(&pool, usize::MAX).is_err());
    assert!(resolve(&pool, "\"Work").is_err());
    pool.accounts[1].label = Some("Work".into());
    assert!(resolve(&pool, "Work").unwrap_err().contains("Ambiguous"));
}

#[test]
fn mobile_account_detail_reports_relative_reset_and_observation_time() {
    let text = detail(&pool(), "Work").unwrap();
    assert!(text.contains("Work · Current"));
    assert!(text.contains("Primary: 37% used"));
    assert!(text.contains("Reset: in "));
    assert!(text.contains("Secondary: unknown used"));
    assert!(text.contains("Reset: unknown"));
    assert!(text.contains("Observed: unknown · age unknown"));
    assert!(text.contains("Cached values remain if refresh fails."));
}

#[test]
fn mobile_quota_windows_show_independent_observation_ages() {
    let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let mut pool = pool();
    pool.accounts[0].rate_limits.observed_at = Some(now.timestamp());
    pool.accounts[0].rate_limits.primary_observed_at = Some(now.timestamp() - 300);
    pool.accounts[0].rate_limits.secondary_observed_at = Some(now.timestamp() - 3 * 86400);
    pool.accounts[0]
        .rate_limits
        .primary
        .as_mut()
        .unwrap()
        .resets_at = Some(now.timestamp() - 60);
    pool.accounts[0].rate_limits.secondary = Some(AccountPoolRateLimitWindow {
        used_percent: 62.0,
        resets_at: None,
    });
    insta::assert_snapshot!(
        "mobile_quota_independent_freshness",
        format!(
            "{}\n\n{}",
            list_at(&pool, /*page*/ 1, now).unwrap(),
            detail_at(&pool, "Work", now).unwrap()
        )
    );
    assert_eq!(
        observation_age(Some(now.timestamp() + 60), now),
        "clock skew"
    );
    assert_eq!(observation_age(Some(i64::MAX), now), "age unknown");
}

#[test]
fn mobile_account_detail_marks_idle_primary_window_not_started() {
    let mut pool = pool();
    pool.accounts[0].rate_limits.primary = Some(AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(1_900_000_000),
    });
    let text = detail(&pool, "Work").unwrap();
    assert!(text.contains("Primary: 0% used"));
    assert!(text.contains("Reset: start unconfirmed"));
}

#[test]
fn mobile_account_detail_shows_recent_warmup_failure() {
    let mut pool = pool();
    pool.accounts[0].rate_limits.primary = Some(AccountPoolRateLimitWindow {
        used_percent: 0.0,
        resets_at: Some(1_900_000_000),
    });
    pool.accounts[0].window_warmup = Some(codex_app_server_protocol::AccountPoolWindowWarmup {
        outcome: codex_app_server_protocol::AccountPoolWindowWarmupOutcome::Failed,
        phase: None,
        consecutive_failures: None,
        attempted_at: chrono::Utc::now().timestamp(),
        retry_after: None,
    });
    let text = detail(&pool, "Work").unwrap();
    assert!(text.contains("Warmup: warmup retry ready"));
    assert!(!text.contains("5h confirmed"));
}

#[test]
fn mobile_account_displayed_names_are_valid_selectors() {
    let mut pool = pool();
    pool.accounts[0].label = Some("Work_Pro [personal]".repeat(10));
    assert_eq!(
        resolve(&pool, &label(&pool.accounts[0])).unwrap(),
        &pool.accounts[0]
    );
}

#[test]
fn mobile_account_settings_explain_effective_controls() {
    let mut config = codex_config::AccountPoolConfigToml {
        window_warmup: Some(false),
        max_reset_wait_minutes: Some(120),
        auto_reset_credits: Some(codex_config::AutoResetCredits::WhenPoolExhausted),
        ..Default::default()
    };
    config.preemptive_switch_percent = Some(0.0);
    insta::assert_snapshot!(settings(&config), @"
    Pool settings
    Strategy: fill-first — use preferred accounts first
    Early rotation: off
    Return to preferred: true
    Warmup: off; every 5 min, uses a tiny request
    Wait and resume: on; up to 120 min, cancellable
    Auto reset credits: only when all accounts exhaust and natural reset is more than 60 min away

    Controls
    /account strategy fill-first|earliest-reset
    /account warmup on|off
    /account resume on|off
    /account wait <minutes: 0..1440>
    /account reset-credits never|when-pool-exhausted

    Quota observations are not additive balances. Partial output or unresolved tools may require manual reconciliation.
    ");
}

fn warmup_pool_and_debug() -> (
    AccountPoolReadResponse,
    codex_app_server_protocol::AccountPoolWarmupDebugResponse,
) {
    let mut pool = pool();
    let template = pool.accounts[0].clone();
    let entries = [
        (
            "reserve-id",
            "Reserve",
            40,
            false,
            Some("warmup retry ready"),
            "eligible for quota check and warmup",
        ),
        (
            "travel-id",
            "Travel",
            20,
            false,
            Some("warmup sent; start unconfirmed"),
            "attempt protected; quota-only confirmation or retry later",
        ),
        (
            "secret-work-id",
            "Work",
            100,
            true,
            None,
            "current execution account",
        ),
        (
            "backup-id",
            "Backup",
            30,
            false,
            Some("warmup deferred; check in 0:30"),
            "attempt protected; quota-only confirmation or retry later",
        ),
        (
            "personal-id",
            "Personal",
            10,
            false,
            Some("warmup in progress"),
            "attempt protected; quota-only confirmation or retry later",
        ),
    ];
    pool.accounts = entries
        .iter()
        .map(|(id, name, priority, is_active, _, _)| {
            let mut account = template.clone();
            account.profile_id = (*id).into();
            account.label = Some((*name).into());
            account.email = Some(format!("{id}@example.com"));
            account.priority = *priority;
            account.is_active = *is_active;
            account.rate_limits.primary = Some(AccountPoolRateLimitWindow {
                used_percent: if *is_active { 37.0 } else { 0.0 },
                resets_at: Some(1_900_000_000),
            });
            account
        })
        .collect();
    let debug = codex_app_server_protocol::AccountPoolWarmupDebugResponse {
        enabled: true,
        pool_enabled: Some(true),
        task_running: true,
        pass_requested: false,
        interval_seconds: 300,
        settle_seconds: 30,
        rotation_strategy: "fillFirst".into(),
        session_model: None,
        accounts: entries
            .into_iter()
            .map(|(id, name, priority, is_active, status, reason)| {
                codex_app_server_protocol::AccountPoolWarmupDebugAccount {
                    profile_id: id.into(),
                    label: Some(name.into()),
                    email: Some(format!("{id}@example.com")),
                    priority,
                    is_active,
                    is_candidate: reason == "eligible for quota check and warmup",
                    availability: "available".into(),
                    primary_used_percent: Some(if is_active { 37.0 } else { 0.0 }),
                    status: status.map(str::to_string),
                    candidate_reason: Some(reason.into()),
                    attempted_at: status.map(|_| 1_789_632_030),
                    retry_after: None,
                    persisted_warmup_outcome: None,
                }
            })
            .collect(),
        events: Vec::new(),
    };
    (pool, debug)
}

#[test]
fn mobile_warmup_status_pages_show_labels_phases_and_candidate_reasons() {
    let (pool, debug) = warmup_pool_and_debug();
    insta::assert_snapshot!(
        "mobile_warmup_page_one",
        warmup_status(&pool, &debug, 1).unwrap()
    );
    insta::assert_snapshot!(
        "mobile_warmup_page_two",
        warmup_status(&pool, &debug, 2).unwrap()
    );
}

#[test]
fn mobile_warmup_status_reports_off_and_requested_pass() {
    let (mut pool, mut debug) = warmup_pool_and_debug();
    pool.accounts.retain(|account| account.is_active);
    debug.accounts.retain(|account| account.is_active);
    debug.enabled = false;
    debug.task_running = false;
    insta::assert_snapshot!(
        "mobile_warmup_off",
        warmup_status(&pool, &debug, 1).unwrap()
    );

    debug.enabled = true;
    debug.task_running = true;
    debug.pass_requested = true;
    insta::assert_snapshot!(
        "mobile_warmup_now_requested",
        warmup_status(&pool, &debug, 1).unwrap()
    );
}
