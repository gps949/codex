use chrono::TimeZone;
use chrono::Utc;
use pretty_assertions::assert_eq;

use super::WindowWarmupDebugKind;
use super::WindowWarmupDebugLog;

#[test]
fn debug_kind_lines_are_single_line_and_include_profile() {
    assert_eq!(WindowWarmupDebugKind::TaskSpawned.line(), "task spawned");
    assert_eq!(
        WindowWarmupDebugKind::PassNoCandidate.line(),
        "pass skipped: no idle standby"
    );
    assert_eq!(
        WindowWarmupDebugKind::RequestStart {
            profile_id: "acct-1".to_string(),
            model: "gpt-5.6-sol".to_string(),
            effort: "medium".to_string(),
            use_responses_lite: true,
        }
        .line(),
        "request start profile=acct-1 model=gpt-5.6-sol effort=medium lite=true"
    );
    assert_eq!(
        WindowWarmupDebugKind::Noop {
            profile_id: "acct-1".to_string(),
            stream_started: false,
            get_primary: Some("0".to_string()),
        }
        .line(),
        "noop profile=acct-1 stream_started=false get_primary=0"
    );
}

#[test]
fn debug_log_drops_oldest_events_when_full() {
    let mut log = WindowWarmupDebugLog::new();
    let at = Utc.with_ymd_and_hms(2026, 9, 17, 0, 0, 0).unwrap();
    for index in 0..81 {
        log.record_at(
            at,
            if index == 0 {
                WindowWarmupDebugKind::TaskSpawned
            } else {
                WindowWarmupDebugKind::PassBegin
            },
        );
    }
    let events = log.events();
    assert_eq!(events.len(), 80);
    assert_eq!(events[0].message, "pass begin");
    assert_eq!(events[79].message, "pass begin");
}
