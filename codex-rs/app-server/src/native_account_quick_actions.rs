//! Reversible choices keep the frozen identity and real-turn cancellation checks.

use super::*;

impl NativeMenuSession {
    pub(super) async fn execute_quick(
        &mut self,
        action: MenuAction,
        manager: &Arc<AccountManager>,
        language: NativeAccountLanguage,
        context: &crate::account_management::AccountOperationContext,
    ) -> anyhow::Result<MenuOutcome> {
        let fresh = FrozenAccountInventory::from_inventory(manager.inventory().await?);
        let operation = match action {
            MenuAction::SelectSubscription(index) => {
                let target = self.account(index)?.clone();
                anyhow::ensure!(
                    matches!(target.detail, AccountDetail::Subscription { .. })
                        && !target.disabled
                        && target.login_state == "signedIn"
                        && matches!(target.state.as_str(), "ready" | "paused"),
                    "This subscription is unavailable. Inspect its account actions before selecting it."
                );
                validate_target(&target, &fresh)?;
                self.validate_identity(
                    &target,
                    self.identities.get(&target.id).and_then(Option::as_deref),
                    manager,
                )?;
                let current = fresh
                    .accounts
                    .iter()
                    .find(|account| account.id == target.id)
                    .ok_or_else(|| anyhow::anyhow!("Subscription no longer appears"))?;
                anyhow::ensure!(
                    matches!(current.state.as_str(), "ready" | "paused"),
                    "Subscription availability changed. Refresh before selecting it."
                );
                AccountManagerOperation::Use {
                    profile_id: target.id,
                }
            }
            MenuAction::Automatic => AccountManagerOperation::Automatic,
            MenuAction::SetStrategy(strategy) => AccountManagerOperation::Settings {
                values: serde_json::json!({"rotation_strategy": strategy}),
            },
            _ => anyhow::bail!("This action requires explicit confirmation"),
        };
        validate_settings(&operation, &self.inventory, &fresh)?;
        context.ensure_current().await?;
        let result = manager.execute_with_context(operation, context).await?;
        self.reload(manager).await?;
        if let MenuAction::SetStrategy(expected) = action {
            anyhow::ensure!(
                self.inventory.rotation_strategy()? == expected,
                "The requested strategy was saved, but a higher-priority configuration keeps another strategy active."
            );
        }
        Ok(MenuOutcome::Completed(
            dialogs::receipt(&result.message, language).into(),
        ))
    }
}
