//! Owner-bound manual reset redemption and recovery for account management.

use super::*;
use crate::reset_credit_journal;
use crate::reset_credit_journal::ManualResetCreditJournal;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_backend_client::ConsumeRateLimitResetCreditCode;

impl AccountManager {
    pub(in crate::account_management) async fn read_credits(
        &self,
        id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let profile = self.profile(id)?;
        let (client, manager, auth) = self.profile_client(id).await?;
        let owner = manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation;
        let details = tokio::time::timeout(
            Duration::from_secs(/*secs*/ 10),
            client.list_rate_limit_reset_credits(),
        )
        .await
        .unwrap_or_else(|error| Err(error.into()));
        manager.reload().await;
        let current_profile = self.profile(id)?;
        let current = manager.auth_cached();
        let pool = self.execution_pool().await?;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready
                && current_profile.profile.credential_home == profile.profile.credential_home
                && manager
                    .auth_change_state_receiver()
                    .borrow()
                    .owner_generation
                    == owner
                && current
                    .as_ref()
                    .is_some_and(|current| current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id())
                && pool.as_ref().is_some_and(|pool| store
                    .validate_profile_auth(pool, &profile.profile.id, &auth)
                    .unwrap_or(false)),
            "Account identity changed during the credit check; refresh before viewing credits"
        );
        let owner_key = reset_credit_journal::owner_key(&self.config.chatgpt_base_url, &auth)
            .map_err(anyhow::Error::msg)?;
        let pending = ManualResetCreditJournal::load(&self.config.codex_home)
            .map_err(anyhow::Error::msg)?
            .pending_for_owner(&owner_key);
        let (available_count, credits, inventory_error) = match details {
            Ok(details) => (
                Some(details.available_count),
                details.credits.into_iter().map(|credit| serde_json::json!({
                    "id":credit.id,"resetType":credit.reset_type,"status":credit.status,"grantedAt":credit.granted_at,
                    "expiresAt":credit.expires_at,"title":credit.title,"description":credit.description,
                })).collect::<Vec<_>>(),
                None,
            ),
            Err(error) => {
                if pending.is_none() {
                    return Err(error);
                }
                let inventory_error = error.to_string().chars()
                    .filter(|character| !character.is_control()).take(/*n*/ 240).collect::<String>();
                (None, Vec::new(), Some(inventory_error))
            }
        };
        Ok(
            serde_json::json!({"profileId":id,"availableCount":available_count,
                "credits":credits,"resetOwnerKey":owner_key,"pendingResetCredit":pending,
                "inventoryError":inventory_error}),
        )
    }

    pub(in crate::account_management) async fn redeem_credit(
        &self,
        id: &str,
        credit_id: &str,
        key: &str,
        expected_owner_key: Option<&str>,
        context: &AccountOperationContext,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !credit_id.is_empty() && credit_id.len() <= 256 && !key.is_empty() && key.len() <= 128,
            "Invalid credit or operation ID"
        );
        anyhow::ensure!(
            expected_owner_key.is_none_or(reset_credit_journal::valid_owner_key),
            "Expected reset owner key is invalid"
        );
        let profile = self.profile(id)?;
        let (client, manager, auth) = self.profile_client(id).await?;
        let owner_key = reset_credit_journal::owner_key(&self.config.chatgpt_base_url, &auth)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            expected_owner_key.is_none_or(|expected| expected == owner_key),
            "Account changed since reset confirmation; retry the original operation for its owner"
        );
        let owner = manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation;
        let store = AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 10);
        let _lock = loop {
            if let Some(lock) = store.try_lock_reset_credit()? {
                break lock;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "Another reset operation is running; retry with the same operation ID"
            );
            tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
        };
        let current_profile = self.profile(id)?;
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready
                && current_profile.profile.credential_home == profile.profile.credential_home,
            "Account selection changed while waiting; refresh before redeeming a credit"
        );
        manager.reload().await;
        anyhow::ensure!(
            manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                == owner
                && manager
                    .auth_cached()
                    .is_some_and(|current| current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()),
            "Account credentials changed while waiting; refresh before redeeming a credit"
        );
        let mut journal =
            ManualResetCreditJournal::load(&self.config.codex_home).map_err(anyhow::Error::msg)?;
        let known = journal
            .known_credit(&owner_key, key, Some(credit_id))
            .map_err(anyhow::Error::msg)?
            .is_some();
        if journal
            .terminal_outcome(&owner_key, key)
            .map_err(anyhow::Error::msg)?
            .is_some()
        {
            context.ensure_current().await?;
            return Ok(
                "Original reset operation already completed. Refresh quota for its current status."
                    .into(),
            );
        }
        journal
            .check_pending(&owner_key, key)
            .map_err(anyhow::Error::msg)?;
        let mut expires_at = None;
        if !known {
            let credits =
                tokio::time::timeout_at(deadline, client.list_rate_limit_reset_credits()).await??;
            let credit = credits
                .credits
                .iter()
                .find(|credit| credit.id == credit_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "This credit is no longer listed. Refresh quota and inspect available credits."
                    )
                })?;
            anyhow::ensure!(
                credit.reset_type == "codex_rate_limits"
                    && credit.status == "available"
                    && credit
                        .expires_at
                        .as_ref()
                        .is_none_or(|expires| DateTime::parse_from_rfc3339(expires)
                            .is_ok_and(|expires| expires > Utc::now())),
                "This credit is unavailable, expired, or has a different quota scope"
            );
            expires_at = credit
                .expires_at
                .as_deref()
                .and_then(|expires| DateTime::parse_from_rfc3339(expires).ok());
        }
        let current_profile = self.profile(id)?;
        manager.reload().await;
        anyhow::ensure!(
            !current_profile.profile.disabled
                && current_profile.state == AccountProfileState::Ready
                && current_profile.profile.credential_home == profile.profile.credential_home
                && manager
                    .auth_change_state_receiver()
                    .borrow()
                    .owner_generation
                    == owner
                && manager.auth_cached().is_some_and(|current| {
                    current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()
                }),
            "Account changed during the credit check; no credit was redeemed"
        );
        let pool = self.execution_pool().await?;
        anyhow::ensure!(
            pool.as_ref().is_some_and(|pool| store
                .validate_profile_auth(pool, &profile.profile.id, &auth)
                .unwrap_or(false)),
            "Account credentials changed; refresh before redeeming this credit"
        );
        if !known && let Some(pool) = pool.as_ref() {
            // This caller already owns the spending lock; historical receipts never clear quota.
            codex_login::reconcile_reset_credit_recovery(pool, &store, &store.load()?).await?;
        }
        let probe = pool
            .as_ref()
            .map(|pool| store.capture_quota_probe(pool, &profile.profile.id, &auth))
            .transpose()?
            .flatten();
        context.ensure_current().await?;
        anyhow::ensure!(
            expires_at.is_none_or(|expires| expires > Utc::now()),
            "Reset credit expired before spending; refresh available credits"
        );
        journal
            .remember(&owner_key, key, credit_id)
            .map_err(anyhow::Error::msg)?;
        let result = tokio::time::timeout_at(
            deadline,
            client.consume_rate_limit_reset_credit_by_id(key, credit_id),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Reset outcome is unconfirmed. Refresh quota before retrying the same operation ID."
            )
        })??;
        let outcome = match result.code {
            ConsumeRateLimitResetCreditCode::Reset => {
                ConsumeAccountRateLimitResetCreditOutcome::Reset
            }
            ConsumeRateLimitResetCreditCode::NothingToReset => {
                ConsumeAccountRateLimitResetCreditOutcome::NothingToReset
            }
            ConsumeRateLimitResetCreditCode::NoCredit => {
                ConsumeAccountRateLimitResetCreditOutcome::NoCredit
            }
            ConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
            }
        };
        journal.complete(&owner_key, key, outcome).map_err(|_| {
            anyhow::anyhow!("Reset response could not be saved; retry the original operation")
        })?;
        manager.reload().await;
        let current = manager.auth_cached();
        anyhow::ensure!(
            manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                == owner
                && current
                    .as_ref()
                    .is_some_and(|current| current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()),
            "Account identity changed during reset; its new identity was left untouched"
        );
        let message = match result.code {
            ConsumeRateLimitResetCreditCode::Reset if result.windows_reset >= 2 => {
                let confirmed = if let (Some(pool), Some(probe)) = (pool.as_ref(), probe) {
                    store.confirm_quota_reset(pool, probe, Utc::now())?
                } else {
                    false
                };
                if confirmed {
                    "Reset credit redeemed and quota recovery confirmed for this account."
                } else {
                    "Reset credit redeemed. Checking fresh quota before confirming local recovery."
                }
            }
            ConsumeRateLimitResetCreditCode::Reset => {
                "Backend reported a partial or unconfirmed reset. Checking fresh quota."
            }
            ConsumeRateLimitResetCreditCode::NothingToReset
            | ConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                "Backend reported no new redemption. Checking quota before confirming recovery."
            }
            ConsumeRateLimitResetCreditCode::NoCredit => {
                "No reset was applied. Refresh quota or inspect available credits."
            }
        };
        drop(_lock);
        self.refresh_profiles(Some(vec![id.into()])).await?;
        Ok(message.into())
    }
}
