use super::render_warmup_debug_lines;
use codex_app_server_protocol::AccountPoolWarmupDebugAccount;
use codex_app_server_protocol::AccountPoolWarmupDebugEvent;
use codex_app_server_protocol::AccountPoolWarmupDebugResponse;
use ratatui::text::Line;

fn render_to_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn warmup_debug_dump_includes_persisted_attempts_and_process_events() {
    let rendered = render_to_text(&render_warmup_debug_lines(
        &AccountPoolWarmupDebugResponse {
            enabled: true,
            pool_enabled: Some(true),
            task_running: true,
            pass_requested: false,
            interval_seconds: 300,
            settle_seconds: 30,
            rotation_strategy: "earliestReset".to_string(),
            session_model: Some("gpt-5.6-sol".to_string()),
            accounts: vec![
                AccountPoolWarmupDebugAccount {
                    profile_id: "acct-current".to_string(),
                    email: Some("current@example.com".to_string()),
                    label: Some("Personal".to_string()),
                    priority: 40,
                    is_active: true,
                    is_candidate: false,
                    availability: "available".to_string(),
                    primary_used_percent: Some(0.0),
                    status: None,
                    candidate_reason: Some("current account".to_string()),
                    attempted_at: None,
                    retry_after: None,
                    persisted_warmup_outcome: None,
                },
                AccountPoolWarmupDebugAccount {
                    profile_id: "acct-standby".to_string(),
                    email: Some("standby@example.com".to_string()),
                    label: Some("Work".to_string()),
                    priority: 10,
                    is_active: false,
                    is_candidate: false,
                    availability: "available".to_string(),
                    primary_used_percent: Some(0.0),
                    status: Some("warmup sent; start unconfirmed".to_string()),
                    candidate_reason: Some("confirmation-only; generation protected".to_string()),
                    attempted_at: Some(1_789_632_030),
                    retry_after: Some(1_789_650_030),
                    persisted_warmup_outcome: Some("failed/unconfirmed".to_string()),
                },
            ],
            events: vec![
                AccountPoolWarmupDebugEvent {
                    at: 1_789_632_000,
                    message: "task spawned".to_string(),
                },
                AccountPoolWarmupDebugEvent {
                    at: 1_789_632_030,
                    message: "pass begin".to_string(),
                },
            ],
        },
    ));

    insta::assert_snapshot!(rendered.as_str());
    assert!(rendered.contains("window_warmup = true"));
    assert!(rendered.contains("rotation_strategy = earliestReset"));
    assert!(rendered.contains("Personal current"));
    assert!(rendered.contains("Work standby"));
    assert!(rendered.contains("confirmation-only; generation protected"));
    assert!(rendered.contains("warmup sent; start unconfirmed"));
    assert!(rendered.contains("task spawned"));
    assert!(rendered.contains("Events (this process; oldest first)"));
    assert!(rendered.contains("Weekly resets are separate"));
}

#[test]
fn warmup_debug_dump_empty_events_explains_settle() {
    let rendered = render_to_text(&render_warmup_debug_lines(
        &AccountPoolWarmupDebugResponse {
            enabled: true,
            pool_enabled: Some(true),
            task_running: true,
            pass_requested: true,
            interval_seconds: 300,
            settle_seconds: 30,
            rotation_strategy: "fillFirst".to_string(),
            session_model: None,
            accounts: Vec::new(),
            events: Vec::new(),
        },
    ));
    assert!(rendered.contains("<none>"));
    assert!(rendered.contains("First automatic pass waits for settle"));
    assert!(rendered.contains("Pass requested; run /warmup again after it finishes."));
    assert!(!rendered.contains("task spawned"));
}

#[test]
fn warmup_debug_shows_disabled_setting_separately_from_initialized_pool() {
    let rendered = render_to_text(&render_warmup_debug_lines(
        &AccountPoolWarmupDebugResponse {
            enabled: false,
            pool_enabled: Some(true),
            task_running: false,
            pass_requested: false,
            interval_seconds: 300,
            settle_seconds: 30,
            rotation_strategy: "fillFirst".to_string(),
            session_model: None,
            accounts: Vec::new(),
            events: Vec::new(),
        },
    ));
    assert!(
        rendered.contains("window_warmup = false\n  pool_enabled = true\n  task_running = false")
    );
    assert!(rendered.contains("respects off/backoff and may use quota"));
    insta::assert_snapshot!(rendered);
}
