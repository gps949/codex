use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

use super::*;

fn user_history(metadata: Option<CodexHarnessMetadata>) -> Vec<ResponseItemEnvelope> {
    vec![ResponseItemEnvelope {
        item: ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "portable user context".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        metadata,
    }]
}

#[test]
fn stock_history_with_execution_provenance_requires_portable_compaction() {
    let provenance = CodexHarnessMetadata {
        execution_profile_id: Some("profile-a".to_string()),
        execution_generation: Some(7),
        ..CodexHarnessMetadata::default()
    };

    assert_eq!(
        PortableCompactionPolicy::for_history(
            &ExecutionAuthMode::Stock,
            &user_history(Some(provenance)),
        ),
        PortableCompactionPolicy::Portable,
    );
    assert_eq!(
        PortableCompactionPolicy::for_history(&ExecutionAuthMode::Stock, &user_history(None)),
        PortableCompactionPolicy::Stock,
    );
}
