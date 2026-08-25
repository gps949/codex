use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_protocol::models::ResponseItem;

#[test]
fn legacy_harness_metadata_deserializes_without_execution_provenance() {
    let metadata: CodexHarnessMetadata =
        serde_json::from_str(r#"{"client_authored":true}"#).expect("legacy metadata should parse");

    assert!(metadata.client_authored);
    assert_eq!(metadata.execution_profile_id, None);
    assert_eq!(metadata.execution_generation, None);
}

#[test]
fn execution_provenance_round_trips_through_rollout_wire() {
    let item = RolloutItem::ResponseItem(ResponseItemEnvelope {
        item: ResponseItem::Message {
            id: Some("resp-account-a".to_string()),
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        metadata: Some(CodexHarnessMetadata {
            client_authored: false,
            execution_profile_id: Some("account-a".to_string()),
            execution_generation: Some(17),
        }),
    });

    let serialized = serde_json::to_string(&item).expect("rollout should serialize");
    let decoded: RolloutItem = serde_json::from_str(&serialized).expect("rollout should parse");

    let RolloutItem::ResponseItem(envelope) = decoded else {
        panic!("expected response item");
    };
    let metadata = envelope.metadata.expect("metadata should survive");
    assert_eq!(metadata.execution_profile_id.as_deref(), Some("account-a"));
    assert_eq!(metadata.execution_generation, Some(17));
}

#[test]
fn empty_execution_provenance_is_omitted_from_metadata_json() {
    let metadata = CodexHarnessMetadata {
        client_authored: false,
        execution_profile_id: None,
        execution_generation: None,
    };

    let value = serde_json::to_value(metadata).expect("metadata should serialize");
    assert!(value.get("execution_profile_id").is_none());
    assert!(value.get("execution_generation").is_none());
}
