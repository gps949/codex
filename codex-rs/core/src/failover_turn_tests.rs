use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::CodexErrorInfo;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn non_account_failure_directive_is_stable() {
    assert!(matches!(
        SamplingFailoverDirective::NotHandled,
        SamplingFailoverDirective::NotHandled
    ));
}

#[test]
fn pool_unavailable_maps_to_usage_limit_exceeded() {
    // Empty ExecutionAuth (no pool) still produces UsageLimitReached so clients get the
    // cooldown-oriented protocol error instead of BadRequest from UnsupportedOperation.
    let execution_auth = ExecutionAuth::shared(AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("test"),
    ));
    let err = pool_unavailable_error(&execution_auth);
    assert!(matches!(
        err.details(),
        CodexErrorDetails::UsageLimitReached(_)
    ));
    assert_eq!(
        err.to_codex_protocol_error(),
        CodexErrorInfo::UsageLimitExceeded
    );
}

#[test]
fn earliest_exhausted_reset_prefers_shorter_future_window_reset_over_reprobe() {
    let pool = codex_login::AccountPool::new();
    let id = codex_login::AccountProfileId::new("seat-a").unwrap();
    pool.register(
        codex_login::AccountProfile::new(
            id.clone(),
            "seat-a".into(),
            /*priority*/ 0,
            Some("Work".into()),
        ),
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    )
    .unwrap();
    let now = Utc::now();
    let window_reset = now + chrono::Duration::seconds(45);
    let reprobe = now + chrono::Duration::minutes(10);
    pool.update_rate_limits(
        &id,
        codex_login::AccountRateLimits {
            primary: Some(codex_login::AccountRateLimitWindow {
                used_percent: 99.0,
                resets_at: Some(window_reset),
                window_minutes: Some(300),
            }),
            secondary: None,
            observed_at: Some(now),
            window_observed_at: None,
        },
    )
    .unwrap();
    let lease = pool.activate(&id).unwrap();
    pool.mark_exhausted_for_recovery(
        &lease,
        codex_login::AccountQuotaRecovery::Reprobe { retry_at: reprobe },
        /*rate_limits*/ None,
    )
    .unwrap();

    assert_eq!(earliest_exhausted_reset_in_pool(&pool), Some(window_reset));
}

#[test]
fn active_account_rate_limit_snapshot_and_messages_use_human_labels() {
    let pool = codex_login::AccountPool::new();
    let id = codex_login::AccountProfileId::new("acct-backup").unwrap();
    pool.register(
        codex_login::AccountProfile::new(
            id.clone(),
            "acct-backup".into(),
            /*priority*/ 0,
            Some("Team Backup".into()),
        ),
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    )
    .unwrap();
    let now = Utc::now();
    let reset = now + chrono::Duration::hours(1);
    pool.update_rate_limits(
        &id,
        codex_login::AccountRateLimits {
            primary: Some(codex_login::AccountRateLimitWindow {
                used_percent: 12.5,
                resets_at: Some(reset),
                window_minutes: Some(300),
            }),
            secondary: None,
            observed_at: Some(now),
            window_observed_at: None,
        },
    )
    .unwrap();
    let lease = pool.activate(&id).unwrap();
    assert_eq!(lease.notice_label(), "`Team Backup` (`acct-backup`)");

    let snapshot = active_pool_rate_limit_snapshot(&pool).expect("active snapshot");
    assert_eq!(snapshot.limit_id.as_deref(), Some("codex"));
    assert_eq!(
        snapshot.primary.as_ref().map(|w| w.used_percent),
        Some(12.5)
    );
    assert_eq!(snapshot.spend_control_reached, None);
    assert_eq!(snapshot.credits, None);

    let rescue = pool_reset_credit_rescue_message(Some(&pool), Some(&id), &id, true);
    assert!(
        rescue.contains(
            "Redeemed one rate-limit reset credit on Codex account `Team Backup` (`acct-backup`)"
        ),
        "{rescue}"
    );
}
