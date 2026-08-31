use std::fmt;

use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;

use crate::context::AccountTransitionToolOutputNotice;
use crate::context::ContextualUserFragment;
use crate::execution_auth::ExecutionAuthLease;

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
}

/// Request-side ownership of one persisted history item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HistoryItemOwnership {
    /// Locally authored harness context that is independent of execution credentials.
    Portable,
    /// Model or tool state stamped with the exact execution identity that produced it.
    ExecutionScoped {
        profile_id: String,
        generation: Option<u64>,
    },
    /// Unattributed model or server state from before execution provenance existed.
    LegacyRootScoped,
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
    pub(crate) fn pooled(
        lease: &ExecutionAuthLease,
        legacy_unattributed_profile_id: Option<String>,
    ) -> Self {
        Self {
            target_profile_id: lease.profile_id().map(|id| id.as_str().to_string()),
            target_generation: lease.generation(),
            legacy_unattributed_profile_id,
        }
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
        if !self.history_requires_projection(&history) {
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
            let (source_profile_id, source_generation) = match history_item_ownership(&envelope) {
                HistoryItemOwnership::Portable => {
                    output.push(envelope.into_item());
                    continue;
                }
                HistoryItemOwnership::ExecutionScoped {
                    profile_id,
                    generation,
                } => (Some(profile_id), generation),
                HistoryItemOwnership::LegacyRootScoped => {
                    (self.legacy_unattributed_profile_id.clone(), None)
                }
            };

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

    pub(crate) fn history_requires_projection(&self, history: &[ResponseItemEnvelope]) -> bool {
        history
            .iter()
            .any(|envelope| match history_item_ownership(envelope) {
                HistoryItemOwnership::Portable => false,
                HistoryItemOwnership::ExecutionScoped {
                    profile_id,
                    generation,
                } => {
                    Some(profile_id.as_str()) != self.target_profile_id.as_deref()
                        || generation.is_some_and(|generation| generation != self.target_generation)
                }
                HistoryItemOwnership::LegacyRootScoped => {
                    self.legacy_unattributed_profile_id.as_deref()
                        != self.target_profile_id.as_deref()
                }
            })
    }
}

fn history_item_ownership(envelope: &ResponseItemEnvelope) -> HistoryItemOwnership {
    if let Some(profile_id) = envelope
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.execution_profile_id.as_ref())
    {
        return HistoryItemOwnership::ExecutionScoped {
            profile_id: profile_id.clone(),
            generation: envelope
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.execution_generation),
        };
    }

    match &envelope.item {
        ResponseItem::AdditionalTools { .. } | ResponseItem::CompactionTrigger { .. } => {
            HistoryItemOwnership::Portable
        }
        ResponseItem::Message { role, .. } => {
            if matches!(role.as_str(), "user" | "developer") {
                HistoryItemOwnership::Portable
            } else {
                HistoryItemOwnership::LegacyRootScoped
            }
        }
        ResponseItem::AgentMessage { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::ContextCompaction { .. }
        | ResponseItem::Other => HistoryItemOwnership::LegacyRootScoped,
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
        output.body = FunctionCallOutputBody::Text(AccountTransitionToolOutputNotice.render());
    }
}

#[cfg(test)]
#[path = "account_transition_tests.rs"]
mod tests;
