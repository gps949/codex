use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;

use crate::execution_auth::ExecutionAuthLease;

/// Whether persisted history can be sent to another execution identity without first asking the
/// current account to turn an opaque remote-compaction checkpoint into portable plaintext history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PortableHistoryStatus {
    Portable,
    /// The active account owns at least one opaque compaction checkpoint. Migrate it before normal
    /// sampling while this account can still decrypt the checkpoint.
    MigrationRequired,
    /// An opaque checkpoint belongs to another account already. It cannot safely be summarized by
    /// the current account and automatic continuation must fail closed rather than erase context.
    BlockedByForeignOpaqueCompaction {
        source_profile_id: Option<String>,
        active_profile_id: Option<String>,
    },
}

/// Inspects durable annotated history before a pooled session starts consuming quota.
///
/// Legacy rollout items written before execution provenance are conservatively attributed to the
/// original root profile supplied by `legacy_unattributed_profile_id`.
pub(crate) fn portable_history_status(
    history: &[ResponseItemEnvelope],
    active_lease: &ExecutionAuthLease,
    legacy_unattributed_profile_id: Option<&str>,
) -> PortableHistoryStatus {
    let active_profile_id = active_lease.profile_id().map(|id| id.as_str());
    let mut migration_required = false;

    for envelope in history {
        if !is_opaque_compaction(&envelope.item) {
            continue;
        }

        let source_profile_id = envelope
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.execution_profile_id.as_deref())
            .or(legacy_unattributed_profile_id);

        if source_profile_id == active_profile_id {
            migration_required = true;
            continue;
        }

        return PortableHistoryStatus::BlockedByForeignOpaqueCompaction {
            source_profile_id: source_profile_id.map(str::to_string),
            active_profile_id: active_profile_id.map(str::to_string),
        };
    }

    if migration_required {
        PortableHistoryStatus::MigrationRequired
    } else {
        PortableHistoryStatus::Portable
    }
}

fn is_opaque_compaction(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Compaction {
            encrypted_content, ..
        } => !encrypted_content.is_empty(),
        ResponseItem::ContextCompaction {
            encrypted_content, ..
        } => encrypted_content.is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_messages_are_portable() {
        let history = vec![ResponseItemEnvelope::new(ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        })];
        // Lease construction is covered by integration tests because it requires an AuthManager.
        assert!(!is_opaque_compaction(&history[0].item));
    }

    #[test]
    fn encrypted_compaction_is_detected() {
        let item = ResponseItem::Compaction {
            id: None,
            encrypted_content: "opaque".to_string(),
            internal_chat_message_metadata_passthrough: None,
        };
        assert!(is_opaque_compaction(&item));
    }
}
