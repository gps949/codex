use super::project_api_history;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn api_projection_strips_unattributed_subscription_encryption_without_rewriting_history() {
    let reasoning: ResponseItem = serde_json::from_value(json!({
        "type": "reasoning", "id": "subscription-reasoning",
        "summary": [{"type": "summary_text", "text": "Keep this readable context"}],
        "encrypted_content": "subscription-only-state",
    }))
    .unwrap();
    let history = vec![ResponseItemEnvelope::new(reasoning)];
    let original = history.clone();
    let projected = project_api_history(history.clone()).unwrap();
    assert_eq!(history, original);
    assert_eq!(
        serde_json::to_value(&projected[0]).unwrap(),
        json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "Keep this readable context"}],
            "content": null,
            "encrypted_content": null,
        })
    );
}

#[test]
fn api_projection_keeps_completed_tool_output_and_strips_account_affinity() {
    let call: ResponseItem = serde_json::from_value(json!({
        "type": "function_call", "id": "subscription-call", "call_id": "completed-call",
        "name": "update_plan", "arguments": "{}", "encrypted_function_args": ["opaque-arguments"],
    }))
    .unwrap();
    let output: ResponseItem = serde_json::from_value(json!({
        "type": "function_call_output", "call_id": "completed-call", "output": "Plan updated",
    }))
    .unwrap();
    let history = vec![
        ResponseItemEnvelope {
            item: call,
            metadata: Some(CodexHarnessMetadata {
                execution_profile_id: Some("subscription-profile".into()),
                ..Default::default()
            }),
        },
        ResponseItemEnvelope::new(output.clone()),
    ];
    let projected = project_api_history(history).unwrap();
    assert_eq!(projected[1], output);
    assert_eq!(
        serde_json::to_value(&projected[0]).unwrap(),
        json!({
            "type": "function_call", "call_id": "completed-call", "name": "update_plan", "arguments": "{}",
        })
    );
}

#[test]
fn api_projection_blocks_opaque_compaction_before_a_provider_request() {
    let compaction = ResponseItem::Compaction {
        id: None,
        encrypted_content: "subscription-only-compaction".into(),
        internal_chat_message_metadata_passthrough: None,
    };
    let error = project_api_history(vec![ResponseItemEnvelope::new(compaction)]).unwrap_err();
    assert!(matches!(
        error.details(),
        CodexErrorDetails::AccountMigrationRequired(_)
    ));
}
