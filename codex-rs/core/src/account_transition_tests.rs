use super::*;
use crate::context::AccountTransitionToolOutputNotice;
use crate::context::ContextualUserFragment;
use codex_login::AccountProfileId;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ReasoningItemReasoningSummary;
use pretty_assertions::assert_eq;

fn envelope(
    item: ResponseItem,
    profile: Option<&str>,
    generation: Option<u64>,
) -> ResponseItemEnvelope {
    ResponseItemEnvelope {
        item,
        metadata: Some(CodexHarnessMetadata {
            execution_profile_id: profile.map(str::to_string),
            execution_generation: generation,
            ..CodexHarnessMetadata::default()
        }),
    }
}

fn unattributed_message(role: &str, suffix: &str, content_kind: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", suffix)),
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: suffix.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            turn_id: Some(format!("turn-{suffix}")),
            content_item_kinds: Some(vec![ContentItemKind(content_kind.to_string())]),
            ..Default::default()
        }),
    }
}

#[test]
fn portable_unattributed_history_retains_ids_and_content_annotations() {
    let portable = vec![
        unattributed_message("user", "user", "user.text"),
        unattributed_message(
            "developer",
            "developer-context",
            "generic.developer_instructions",
        ),
        unattributed_message(
            "developer",
            "world-state",
            "environment_context.instructions",
        ),
    ];
    let model_or_server = [
        ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", "assistant")),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "model output".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: Some(
                InternalChatMessageMetadataPassthrough {
                    turn_id: Some("turn-assistant".to_string()),
                    ..Default::default()
                },
            ),
        },
        ResponseItem::Reasoning {
            id: Some(ResponseItemId::with_suffix("rs", "reasoning")),
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "readable summary".to_string(),
            }],
            content: None,
            encrypted_content: Some("opaque-reasoning".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCall {
            id: Some(ResponseItemId::with_suffix("fc", "tool-call")),
            name: "example".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            encrypted_function_args: Some(vec!["opaque-arguments".to_string()]),
            call_id: "call-1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: Some(ResponseItemId::with_suffix("fco", "tool-output")),
            call_id: Some("call-1".to_string()),
            name: None,
            namespace: None,
            output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "portable tool text".to_string(),
                },
                FunctionCallOutputContentItem::EncryptedContent {
                    encrypted_content: "opaque-tool-output".to_string(),
                },
            ]),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Compaction {
            id: Some(ResponseItemId::with_suffix("cmp", "empty")),
            encrypted_content: String::new(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let history = portable
        .iter()
        .chain(model_or_server.iter())
        .cloned()
        .map(ResponseItemEnvelope::new)
        .collect();
    let transition = AccountHistoryTransition {
        target_profile_id: Some("managed-target".to_string()),
        target_generation: 9,
        legacy_unattributed_profile_id: Some("legacy-root".to_string()),
    };

    let (projected, _) = transition
        .prepare_for_request(history)
        .expect("unattributed portable history should be projectable");

    assert_eq!(&projected[..portable.len()], portable.as_slice());
    assert!(
        projected[portable.len()..]
            .iter()
            .all(|item| item.id().is_none())
    );
    let ResponseItem::Reasoning {
        encrypted_content, ..
    } = &projected[portable.len() + 1]
    else {
        panic!("expected projected reasoning");
    };
    assert_eq!(encrypted_content, &None);
    let ResponseItem::FunctionCall {
        encrypted_function_args,
        ..
    } = &projected[portable.len() + 2]
    else {
        panic!("expected projected tool call");
    };
    assert_eq!(encrypted_function_args, &None);
    let ResponseItem::FunctionCallOutput { output, .. } = &projected[portable.len() + 3] else {
        panic!("expected projected tool output");
    };
    assert_eq!(
        output.body,
        FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
            text: "portable tool text".to_string(),
        },])
    );
}

#[test]
fn single_profile_foreign_history_is_projected() {
    let foreign = ResponseItem::Reasoning {
        id: Some(ResponseItemId::with_suffix("rs", "foreign")),
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "portable summary".to_string(),
        }],
        content: None,
        encrypted_content: Some("opaque-profile-a-state".to_string()),
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            turn_id: Some("turn-profile-a".to_string()),
            ..Default::default()
        }),
    };
    let history = vec![envelope(foreign, Some("profile-a"), Some(3))];
    let transition = AccountHistoryTransition {
        target_profile_id: Some("profile-b".to_string()),
        target_generation: 1,
        legacy_unattributed_profile_id: None,
    };

    assert!(transition.history_requires_projection(&history));

    let (projected, stats) = transition
        .prepare_for_request(history)
        .expect("foreign history should be projectable with one configured target profile");

    assert_eq!(
        projected,
        vec![ResponseItem::Reasoning {
            id: None,
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "portable summary".to_string(),
            }],
            content: None,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        }]
    );
    assert_eq!(
        stats,
        AccountHistoryTransitionStats {
            cleared_response_ids: 1,
            cleared_internal_metadata: 1,
            stripped_reasoning_blobs: 1,
            ..AccountHistoryTransitionStats::default()
        }
    );
}

// The integration tests in codex-core construct real ExecutionAuthLease values. These unit
// helpers focus on the item sanitizer independently of AuthManager setup.
#[test]
fn foreign_reasoning_drops_opaque_blob_but_keeps_summary() {
    let mut stats = AccountHistoryTransitionStats::default();
    let item = ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "portable summary".to_string(),
        }],
        content: None,
        encrypted_content: Some("opaque-a".to_string()),
        internal_chat_message_metadata_passthrough: None,
    };
    let sanitized = sanitize_foreign_item(item, Some("account-a"), Some("account-b"), &mut stats)
        .expect("foreign reasoning should be sanitizable")
        .expect("summary should keep the reasoning item");
    let ResponseItem::Reasoning {
        encrypted_content, ..
    } = sanitized
    else {
        panic!("expected reasoning item");
    };
    assert_eq!(encrypted_content, None);
    assert_eq!(stats.stripped_reasoning_blobs, 1);
}

#[test]
fn foreign_encrypted_only_tool_output_keeps_call_pair_with_placeholder() {
    let mut stats = AccountHistoryTransitionStats::default();
    let item = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("call-1".to_string()),
        name: None,
        namespace: None,
        output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: "opaque-tool-output".to_string(),
            },
        ]),
        internal_chat_message_metadata_passthrough: None,
    };
    let sanitized = sanitize_foreign_item(item, Some("account-a"), Some("account-b"), &mut stats)
        .expect("tool output should be sanitizable")
        .expect("tool output must remain paired");
    let ResponseItem::FunctionCallOutput { output, .. } = sanitized else {
        panic!("expected tool output");
    };
    assert_eq!(
        output.body,
        FunctionCallOutputBody::Text(AccountTransitionToolOutputNotice.render())
    );
    assert_eq!(stats.stripped_encrypted_tool_outputs, 1);
}

#[test]
fn foreign_opaque_compaction_fails_closed() {
    let mut stats = AccountHistoryTransitionStats::default();
    let error = sanitize_foreign_item(
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "opaque-compaction".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        Some("account-a"),
        Some("account-b"),
        &mut stats,
    )
    .expect_err("opaque compaction must not be silently discarded");
    assert!(matches!(
        error,
        AccountHistoryTransitionError::OpaqueCompaction { .. }
    ));
}

#[test]
fn metadata_fixture_documents_history_wire_extension() {
    let profile = AccountProfileId::new("account-a").expect("valid id");
    let envelope = envelope(
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        Some(profile.as_str()),
        Some(7),
    );
    assert_eq!(
        envelope
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.execution_profile_id.as_deref()),
        Some("account-a")
    );
}
