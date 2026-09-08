use super::*;
use pretty_assertions::assert_eq;

#[test]
fn reset_countdown_uses_compact_explicit_units() {
    assert_eq!(format_reset_countdown(/*remaining_seconds*/ 1), "1m");
    assert_eq!(format_reset_countdown(/*remaining_seconds*/ 60), "1m");
    assert_eq!(
        format_reset_countdown(/*remaining_seconds*/ 3 * 60 * 60 + 46 * 60),
        "3h46m"
    );
    assert_eq!(
        format_reset_countdown(
            /*remaining_seconds*/ 3 * 24 * 60 * 60 + 21 * 60 * 60 + 2 * 60
        ),
        "3d21h02m"
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
            "3h46m".cyan(),
        ]
    );
    assert_eq!(
        account_rate_limit_description(&window, AccountRateLimitKind::Weekly, now),
        vec![
            "38%".magenta(),
            " weekly used".dim(),
            ", reset in ".dim(),
            "3h46m".magenta(),
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
