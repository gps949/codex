//! Frozen targets retain exact identity across fresh inventory and confirmation checks.

use super::*;

impl NativeMenuSession {
    pub(crate) fn bind_targets(&mut self, manager: &AccountManager) {
        self.identities = self
            .inventory
            .accounts
            .iter()
            .filter(|account| matches!(account.detail, AccountDetail::Subscription { .. }))
            .map(|account| {
                (
                    account.id.clone(),
                    manager.profile_identity(&account.id).ok(),
                )
            })
            .collect();
    }

    pub(super) fn validate_identity(
        &self,
        target: &FrozenAccount,
        expected: Option<&str>,
        manager: &AccountManager,
    ) -> anyhow::Result<()> {
        if matches!(target.detail, AccountDetail::Api { .. }) {
            return Ok(());
        }
        match expected {
            Some(expected) => anyhow::ensure!(
                manager.profile_identity(&target.id)?.as_str() == expected,
                "Account user or workspace changed. Reopen the menu before confirming."
            ),
            None => {
                anyhow::ensure!(
                    matches!(target.login_state.as_str(), "pending" | "needsLogin"),
                    "This account has no verifiable saved identity. Reopen its login before changing it."
                );
                anyhow::ensure!(
                    manager.profile_identity(&target.id).is_err(),
                    "Account identity appeared after this menu was opened. Reopen its actions."
                );
            }
        }
        Ok(())
    }
}

pub(super) fn same_target(expected: &FrozenAccount, actual: &FrozenAccount) -> bool {
    if expected.id != actual.id || expected.disabled != actual.disabled {
        return false;
    }
    if expected.login_state != actual.login_state {
        return false;
    }
    match (&expected.detail, &actual.detail) {
        (
            AccountDetail::Subscription {
                email: left,
                plan: left_plan,
                ..
            },
            AccountDetail::Subscription {
                email: right,
                plan: right_plan,
                ..
            },
        ) => left == right && left_plan == right_plan,
        (
            AccountDetail::Api {
                account: left,
                has_key: left_key,
                credential_revision: left_revision,
            },
            AccountDetail::Api {
                account: right,
                has_key: right_key,
                credential_revision: right_revision,
            },
        ) => {
            serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
                && left_key == right_key
                && left_revision == right_revision
        }
        _ => false,
    }
}

pub(super) fn validate_target(
    expected: &FrozenAccount,
    inventory: &FrozenAccountInventory,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        inventory
            .accounts
            .iter()
            .any(|account| same_target(expected, account)),
        "Account identity or API target changed. Reopen its actions before continuing."
    );
    Ok(())
}

pub(super) fn validate_settings(
    operation: &AccountManagerOperation,
    captured: &FrozenAccountInventory,
    fresh: &FrozenAccountInventory,
) -> anyhow::Result<()> {
    match operation {
        AccountManagerOperation::ManualResetReview { .. }
        | AccountManagerOperation::ResetJournalArchive { .. } => {
            super::reset_review::validate_reset_review(operation, captured, fresh)?;
        }
        AccountManagerOperation::RoutingSave {
            expected_version,
            config,
        } => {
            let captured = captured
                .routing
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Model routing snapshot is unavailable"))?;
            let fresh = fresh
                .routing
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Model routing settings could not be reloaded"))?;
            anyhow::ensure!(
                expected_version.as_ref() == Some(&captured.user_config_version)
                    && captured.user_config_version == fresh.user_config_version,
                "Model routing settings changed. Reload before saving."
            );
            if config.source == codex_config::ModelRoutingSource::DecisionService
                && config.send_task_description
            {
                anyhow::ensure!(
                    captured.decision_service_ready && fresh.decision_service_ready,
                    "Decision service changed. Inspect its settings before confirming."
                );
            }
        }
        AccountManagerOperation::ApiFallback { .. } => anyhow::ensure!(
            serde_json::to_value(&captured.fallback)? == serde_json::to_value(&fresh.fallback)?,
            "API fallback changed while this menu was open. Inspect its current settings first."
        ),
        AccountManagerOperation::Settings { values } => {
            if let Some(values) = values.as_object() {
                for key in values.keys() {
                    anyhow::ensure!(
                        captured.settings.get(key) == fresh.settings.get(key),
                        "Pool setting changed while this menu was open. Inspect it before confirming."
                    );
                }
            }
        }
        _ => {}
    }
    Ok(())
}
