use std::fmt;

use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;

use crate::execution_auth::ExecutionAuthLease;

const OMITTED_ENCRYPTED_TOOL_OUTPUT: &str =
    "[Encrypted tool output omitted after Codex execution-account switch.]";

/// Stamps a history envelope with the execution identity that produced account-sensitive model
/// state. The metadata is intentionally history-only and never sent to the Responses API.
pub(crate) fn stamp_execution_provenance(
    metadata: &mut Option<CodexHarnessMetadata>,
    lease: &ExecutionAuthLease,
) {
    let Some(profile_id) = lease.profile_id() else {
        return;
    };
    let metadata = metadata.get_or_insert_with(CodexHarnessMetadata::default);
    metadata.execution_profile_id = Some(profile_id.as_str().to_string());
    metadata.execution_generation = Some(lease.generation());
}

/// Convenience constructor used when a freshly received model item is about to enter durable
/// history. Locally authored user/context items should normally remain unattributed.
pub(crate) fn envelope_from_execution(
    item: ResponseItem,
    lease: &ExecutionAuthLease,
) -> ResponseItemEnvelope {
    let mut envelope = ResponseItemEnvelope::new(item);
    stamp_execution_provenance(&mut envelope.metadata, lease);
    envelope
}

/// Describes how persisted model history should be projected into a request after an execution
/// account transition.
///
/// Durable history is never rewritten by this projection. Only the request-side clone is changed,
/// so the original account can still resume with all of its opaque state if it becomes active again.
#[derive(Clone, Debug)]
pub(crate) struct AccountHistoryTransition {
    target_profile_id: Option<String>,
    target_generation: u64,
    /// Rollouts written before native multi-account support have no execution provenance. When the
    /// pool is bootstrapped from an existing login, those legacy opaque items belong to that root
    /// profile until proven otherwise.
    legacy_unattributed_profile_id: Option<String>,
    /// False for ordinary same-account requests. True only after the logical session has changed
    /// execution profile at least once and cross-account compatibility filtering is required.
    cross_account: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AccountHistoryTransitionStats {
    pub(crate) cleared_response_ids: usize,
    pub(crate) cleared_internal_metadata: usize,
    pub(crate) stripped_reasoning_blobs: usize,
    pub(crate) stripped_encrypted_function_args: usize,
    pub(crate) stripped_encrypted_tool_outputs: usize,
    pub(crate) dropped_encrypted_agent_messages: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum AccountHistoryTransitionError {
    /// Remote compaction is opaque. If it belongs to another account, silently dropping it would
    /// erase the only surviving representation of earlier context. Multi-account mode therefore
    /// requires portable/plaintext compaction before such a switch can be completed.
    OpaqueCompaction {
        source_profile_id: Option<String>,
        target_profile_id: Option<String>,
    },
}

impl fmt::Display for AccountHistoryTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpaqueCompaction {
                source_profile_id,
                target_profile_id,
            } => write!(
                f,
                "cannot move opaque remote compaction from execution account {} to {}; create a portable compaction checkpoint before switching accounts",
                source_profile_id.as_deref().unwrap_or("<unknown>"),
                target_profile_id.as_deref().unwrap_or("<legacy>"),
            ),
        }
    }
}

impl std::error::Error for AccountHistoryTransitionError {}

impl AccountHistoryTransition {
    pub(crate) fn initial(
        lease: &ExecutionAuthLease,
        legacy_unattributed_profile_id: Option<String>,
    ) -> Self {
        Self {
            target_profile_id: lease.profile_id().map(|id| id.as_str().to_string()),
            target_generation: lease.generation(),
            legacy_unattributed_profile_id,
            cross_account: false,
        }
    }

    pub(crate) fn pooled(
        lease: &ExecutionAuthLease,
        legacy_unattributed_profile_id: Option<String>,
    ) -> Self {
        let mut transition = Self::initial(lease, legacy_unattributed_profile_id);
        transition.cross_account = true;
        transition
    }

    /// Converts normalized, annotated history into a request-safe projection for the active
    /// execution identity.
    ///
    /// Same-account items keep encrypted reasoning and other server state. Items known to have
    /// originated from another account retain locally readable semantic content but lose account-
    /// scoped ids, passthrough metadata, encrypted reasoning and encrypted function payloads.
    pub(crate) fn prepare_for_request(
        &self,
        history: Vec<ResponseItemEnvelope>,
    ) -> Result<(Vec<ResponseItem>, AccountHistoryTransitionStats), AccountHistoryTransitionError>
    {
        if !self.cross_account {
            return Ok((
                history
                    .into_iter()
                    .map(ResponseItemEnvelope::into_item)
                    .collect(),
                AccountHistoryTransitionStats::default(),
            ));
        }

        let mut stats = AccountHistoryTransitionStats::default();
        let mut output = Vec::with_capacity(history.len());
        for envelope in history {
            let source_profile_id = envelope
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.execution_profile_id.clone())
                .or_else(|| self.legacy_unattributed_profile_id.clone());
            let source_generation = envelope
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.execution_generation);

            let same_profile = source_profile_id.as_deref() == self.target_profile_id.as_deref();
            if same_profile {
                let mut item = envelope.into_item();
                // A switch away from and later back to the same account may leave response ids and
                // internal routing metadata tied to an older transport generation. The encrypted
                // content itself remains account-compatible, so preserve it while dropping only
                // transport-affine fields.
                if source_generation.is_some_and(|generation| generation != self.target_generation)
                {
                    clear_transport_affinity(&mut item, &mut stats);
                }
                output.push(item);
                continue;
            }

            if let Some(item) = sanitize_foreign_item(
                envelope.into_item(),
                source_profile_id.as_deref(),
                self.target_profile_id.as_deref(),
                &mut stats,
            )? {
                output.push(item);
            }
        }
        Ok((output, stats))
    }
}

fn sanitize_foreign_item(
    mut item: ResponseItem,
    source_profile_id: Option<&str>,
    target_profile_id: Option<&str>,
    stats: &mut AccountHistoryTransitionStats,
) -> Result<Option<ResponseItem>, AccountHistoryTransitionError> {
    clear_transport_affinity(&mut item, stats);

    match &mut item {
        ResponseItem::AgentMessage { content, .. } => {
            let before = content.len();
            content.retain(|part| matches!(part, AgentMessageInputContent::InputText { .. }));
            if content.len() != before && content.is_empty() {
                stats.dropped_encrypted_agent_messages =
                    stats.dropped_encrypted_agent_messages.saturating_add(1);
                return Ok(None);
            }
        }
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => {
            if encrypted_content.take().is_some() {
                stats.stripped_reasoning_blobs = stats.stripped_reasoning_blobs.saturating_add(1);
            }
            let has_readable_content = content.as_ref().is_some_and(|content| !content.is_empty());
            if summary.is_empty() && !has_readable_content {
                return Ok(None);
            }
        }
        ResponseItem::FunctionCall {
            encrypted_function_args,
            ..
        } => {
            if encrypted_function_args.take().is_some() {
                stats.stripped_encrypted_function_args =
                    stats.stripped_encrypted_function_args.saturating_add(1);
            }
        }
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => {
            sanitize_function_output(output, stats);
        }
        ResponseItem::Compaction {
            encrypted_content, ..
        } if !encrypted_content.is_empty() => {
            return Err(AccountHistoryTransitionError::OpaqueCompaction {
                source_profile_id: source_profile_id.map(str::to_string),
                target_profile_id: target_profile_id.map(str::to_string),
            });
        }
        ResponseItem::ContextCompaction {
            encrypted_content: Some(_),
            ..
        } => {
            return Err(AccountHistoryTransitionError::OpaqueCompaction {
                source_profile_id: source_profile_id.map(str::to_string),
                target_profile_id: target_profile_id.map(str::to_string),
            });
        }
        ResponseItem::ContextCompaction {
            encrypted_content: None,
            ..
        }
        | ResponseItem::CompactionTrigger { .. } => return Ok(None),
        ResponseItem::AdditionalTools { .. }
        | ResponseItem::Message { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Other
        | ResponseItem::Compaction { .. } => {}
    }

    Ok(Some(item))
}

fn clear_transport_affinity(item: &mut ResponseItem, stats: &mut AccountHistoryTransitionStats) {
    if item.id().is_some() {
        stats.cleared_response_ids = stats.cleared_response_ids.saturating_add(1);
        item.set_id(None);
    }

    // `InternalChatMessageMetadataPassthrough` is a Responses-side contract. Keep durable history
    // untouched, but do not send metadata produced under another execution identity.
    let before = serde_json::to_value(&*item).ok();
    item.clear_internal_chat_message_metadata_passthrough();
    let after = serde_json::to_value(&*item).ok();
    if before != after {
        stats.cleared_internal_metadata = stats.cleared_internal_metadata.saturating_add(1);
    }
}

fn sanitize_function_output(
    output: &mut codex_protocol::models::FunctionCallOutputPayload,
    stats: &mut AccountHistoryTransitionStats,
) {
    let FunctionCallOutputBody::ContentItems(items) = &mut output.body else {
        return;
    };

    let before = items.len();
    items.retain(|item| !matches!(item, FunctionCallOutputContentItem::EncryptedContent { .. }));
    let removed = before.saturating_sub(items.len());
    stats.stripped_encrypted_tool_outputs = stats
        .stripped_encrypted_tool_outputs
        .saturating_add(removed);

    if items.is_empty() && removed > 0 {
        output.body = FunctionCallOutputBody::Text(OMITTED_ENCRYPTED_TOOL_OUTPUT.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_login::AccountProfileId;
    use codex_protocol::models::ReasoningItemReasoningSummary;

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
        let sanitized =
            sanitize_foreign_item(item, Some("account-a"), Some("account-b"), &mut stats)
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
            call_id: "call-1".to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
                FunctionCallOutputContentItem::EncryptedContent {
                    encrypted_content: "opaque-tool-output".to_string(),
                },
            ]),
            internal_chat_message_metadata_passthrough: None,
        };
        let sanitized =
            sanitize_foreign_item(item, Some("account-a"), Some("account-b"), &mut stats)
                .expect("tool output should be sanitizable")
                .expect("tool output must remain paired");
        let ResponseItem::FunctionCallOutput { output, .. } = sanitized else {
            panic!("expected tool output");
        };
        assert_eq!(
            output.body,
            FunctionCallOutputBody::Text(OMITTED_ENCRYPTED_TOOL_OUTPUT.to_string())
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
}
