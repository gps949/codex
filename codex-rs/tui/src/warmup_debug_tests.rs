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
fn warmup_debug_dump_includes_accounts_events_and_hidden_status_hint() {
    let rendered = render_to_text(&render_warmup_debug_lines(
        &AccountPoolWarmupDebugResponse {
            enabled: true,
            task_running: true,
            pass_requested: false,
            interval_seconds: 300,
            settle_seconds: 30,
            rotation_strategy: "earliestReset".to_string(),
            session_model: Some("gpt-5.6-sol".to_string()),
            accounts: vec![
                AccountPoolWarmupDebugAccount {
                    profile_id: "acct-current".to_string(),
                    email: Some("kangweiye@gmail.com".to_string()),
                    label: None,
                    priority: 40,
                    is_active: true,
                    is_candidate: false,
                    availability: "available".to_string(),
                    primary_used_percent: Some(0.0),
                    persisted_warmup_outcome: None,
                },
                AccountPoolWarmupDebugAccount {
                    profile_id: "acct-standby".to_string(),
                    email: Some("qihuangong@gmail.com".to_string()),
                    label: None,
                    priority: 10,
                    is_active: false,
                    is_candidate: true,
                    availability: "available".to_string(),
                    primary_used_percent: Some(0.0),
                    persisted_warmup_outcome: Some("failed".to_string()),
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
    assert!(rendered.contains("kangweiye@gmail.com current"));
    assert!(rendered.contains("qihuangong@gmail.com standby"));
    assert!(rendered.contains("candidate=yes"));
    assert!(rendered.contains("task spawned"));
    assert!(rendered.contains("Picker hides warmup status"));
}

#[test]
fn warmup_debug_dump_empty_events_explains_settle() {
    let rendered = render_to_text(&render_warmup_debug_lines(
        &AccountPoolWarmupDebugResponse {
            enabled: true,
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
