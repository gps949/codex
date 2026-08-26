use chrono::Duration;
use chrono::Utc;
use codex_config::AutoResetCredits;
use pretty_assertions::assert_eq;

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
