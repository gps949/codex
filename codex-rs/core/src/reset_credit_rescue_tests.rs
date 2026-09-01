use chrono::Duration;
use chrono::Utc;
use codex_config::AutoResetCredits;
use codex_login::AccountPool;
use codex_login::AccountProfile;
use codex_login::AccountProfileId;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;

use super::reactivate_redeemed_profile;
use super::should_redeem;

#[test]
fn never_mode_never_redeems() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::Never,
            Duration::minutes(60),
            /*earliest_reset*/ None,
            now
        ),
        false
    );
}

#[test]
fn nearby_natural_reset_wins_over_a_credit() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            Some(now + Duration::minutes(30)),
            now
        ),
        false
    );
}

#[test]
fn distant_reset_justifies_redeeming() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            Some(now + Duration::hours(4)),
            now
        ),
        true
    );
}

#[test]
fn unknown_reset_justifies_redeeming() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            /*earliest_reset*/ None,
            now
        ),
        true
    );
}

#[test]
fn redeemed_credit_does_not_report_success_when_profile_cannot_reactivate() {
    let pool = AccountPool::new();
    let profile_id = AccountProfileId::new("disabled-after-consume").expect("valid profile id");
    pool.register(
        AccountProfile::new(
            profile_id.clone(),
            std::path::PathBuf::from("/tmp/disabled-after-consume"),
            0,
            /*label*/ None,
        ),
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    )
    .expect("register profile");
    pool.set_disabled(&profile_id, true)
        .expect("disable profile after consume");

    assert!(reactivate_redeemed_profile(&pool, profile_id).is_none());
}
