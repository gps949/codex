use std::fmt;

use codex_history::ResponseItemEnvelope;
use codex_login::AccountProfileId;
use codex_protocol::models::ResponseItem;

use crate::account_transition::HistoryItemOwnership;
use crate::account_transition::history_item_ownership;
use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthBinding;

/// Captured target identity and legacy ownership mapping used by one transition preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccountTransitionTargetProfile {
    profile_id: Option<AccountProfileId>,
    legacy_unattributed_profile_id: Option<AccountProfileId>,
    stock_execution: bool,
}

impl AccountTransitionTargetProfile {
    pub(crate) fn from_execution(
        execution_auth: &ExecutionAuth,
        execution_binding: &ExecutionAuthBinding,
    ) -> Self {
        let (profile_id, stock_execution) = match execution_binding {
            ExecutionAuthBinding::Stock => (None, true),
            ExecutionAuthBinding::Pooled(lease) => (lease.profile_id().cloned(), false),
        };
        Self {
            profile_id,
            legacy_unattributed_profile_id: execution_auth.legacy_unattributed_profile_id(),
            stock_execution,
        }
    }
}

/// Whether the captured target may receive the current history without migrating an opaque
/// compaction checkpoint first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AccountTransitionReadiness {
    Ready,
    MigrationRequired {
        owner_profile_id: Option<AccountProfileId>,
    },
}

impl AccountTransitionReadiness {
    pub(crate) fn ensure_ready(
        self,
        target_profile: &AccountTransitionTargetProfile,
    ) -> Result<(), OpaqueHistoryMigrationRequired> {
        match self {
            Self::Ready => Ok(()),
            Self::MigrationRequired { owner_profile_id } => Err(OpaqueHistoryMigrationRequired {
                owner_profile_id,
                target_profile_id: target_profile.profile_id.clone(),
            }),
        }
    }
}

/// Purely inspects cloned annotated history before any target authentication is bound.
pub(crate) fn preflight_account_transition(
    history: &[ResponseItemEnvelope],
    target_profile: &AccountTransitionTargetProfile,
) -> AccountTransitionReadiness {
    for envelope in history {
        let is_opaque_compaction = match &envelope.item {
            ResponseItem::Compaction {
                encrypted_content, ..
            } => !encrypted_content.is_empty(),
            ResponseItem::ContextCompaction {
                encrypted_content, ..
            } => encrypted_content.is_some(),
            _ => false,
        };
        if !is_opaque_compaction {
            continue;
        }

        let owner_profile_id = match history_item_ownership(envelope) {
            HistoryItemOwnership::ExecutionScoped { profile_id, .. } => {
                AccountProfileId::new(profile_id).ok()
            }
            HistoryItemOwnership::LegacyRootScoped if target_profile.stock_execution => continue,
            HistoryItemOwnership::LegacyRootScoped => {
                target_profile.legacy_unattributed_profile_id.clone()
            }
            HistoryItemOwnership::Portable => None,
        };
        if owner_profile_id
            .as_ref()
            .is_some_and(|owner| Some(owner) == target_profile.profile_id.as_ref())
        {
            continue;
        }
        return AccountTransitionReadiness::MigrationRequired { owner_profile_id };
    }

    AccountTransitionReadiness::Ready
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpaqueHistoryMigrationRequired {
    owner_profile_id: Option<AccountProfileId>,
    target_profile_id: Option<AccountProfileId>,
}

impl fmt::Display for OpaqueHistoryMigrationRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = self
            .target_profile_id
            .as_ref()
            .map(|profile_id| format!("`{profile_id}`"))
            .unwrap_or_else(|| "the selected account".to_string());
        match self.owner_profile_id.as_ref() {
            Some(owner) => write!(
                formatter,
                "History contains an opaque compaction owned by `{owner}`. Switch to `{owner}` and run `/compact` before using {target}."
            ),
            None => write!(
                formatter,
                "History contains an opaque compaction with an unknown owner. Resume the thread with its owning account and run `/compact` before using {target}."
            ),
        }
    }
}

impl std::error::Error for OpaqueHistoryMigrationRequired {}

#[cfg(test)]
#[path = "opaque_history_migration_tests.rs"]
mod tests;
