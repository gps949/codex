//! Bounded credit reads retain an unresolved operation's immutable owner and request key.

use super::*;

impl NativeMenuSession {
    pub(super) fn load_credits(
        &mut self,
        data: serde_json::Value,
        target: Option<FrozenAccount>,
        identity: Option<String>,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<()> {
        let owner_key = data
            .get("resetOwnerKey")
            .and_then(serde_json::Value::as_str)
            .filter(|value| crate::reset_credit_journal::valid_owner_key(value))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Reset credit owner could not be verified. Reload before continuing."
                )
            })?
            .to_string();
        let mut pending_reset: Option<NativePendingReset> = serde_json::from_value(
            data.get("pendingResetCredit")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )?;
        if let Some(original) = &self.pending_reset {
            self.unconfirmed_resets
                .insert(original.owner_key.clone(), original.clone());
        }
        if let Some(original) = self.unconfirmed_resets.get(&owner_key) {
            anyhow::ensure!(
                pending_reset
                    .as_ref()
                    .is_none_or(|current| current == original),
                "The previous reset operation changed. Review its original binding before continuing."
            );
            pending_reset = Some(original.clone());
        }
        if let Some(pending) = &pending_reset {
            anyhow::ensure!(
                pending.owner_key == owner_key
                    && !pending.idempotency_key.is_empty()
                    && pending.idempotency_key.len() <= 128
                    && pending
                        .credit_id
                        .as_ref()
                        .is_none_or(|id| !id.is_empty() && id.len() <= 256),
                "The previous reset operation could not be verified. Reload before continuing."
            );
            if let Some(credit_id) = &pending.credit_id {
                self.redemption_keys.insert(
                    (owner_key.clone(), credit_id.clone()),
                    pending.idempotency_key.clone(),
                );
            }
            self.unconfirmed_resets
                .insert(owner_key.clone(), pending.clone());
        }
        self.credit_inventory_error = data
            .get("inventoryError")
            .and_then(serde_json::Value::as_str)
            .map(|error| bounded_text(error, /*max_chars*/ 240));
        self.pending_reset = pending_reset;
        self.credit_owner_key = Some(owner_key);
        self.credits = data
            .get("credits")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .take(64)
            .filter_map(|credit| {
                let id = credit.get("id")?.as_str()?;
                if id.is_empty() || id.len() > 256 {
                    return None;
                }
                let expires = credit.get("expiresAt").and_then(serde_json::Value::as_str);
                let not_expired = expires.is_none_or(|value| {
                    chrono::DateTime::parse_from_rfc3339(value)
                        .is_ok_and(|at| at > chrono::Utc::now())
                });
                Some(NativeCredit {
                    id: id.into(),
                    expires: expires
                        .unwrap_or(language.text("No expiry listed", "未列出到期时间"))
                        .into(),
                    available: not_expired
                        && credit.get("status").and_then(serde_json::Value::as_str)
                            == Some("available")
                        && credit.get("resetType").and_then(serde_json::Value::as_str)
                            == Some("codex_rate_limits"),
                })
            })
            .collect();
        self.credit_target = target;
        self.credit_identity = identity;
        let index = self
            .credit_target
            .as_ref()
            .and_then(|target| {
                self.inventory
                    .accounts
                    .iter()
                    .position(|account| account.id == target.id)
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Credit owner no longer appears. Reopen its account actions.")
            })?;
        self.page = MenuPage::Credits(index, 0);
        Ok(())
    }

    pub(super) fn prepare_reset_credit(
        &mut self,
        index: usize,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<()> {
        if let Some(previous) = &self.pending_reset {
            if previous.credit_id.is_some() {
                self.prepare(MenuOperation::RetryPendingCredit(index), language)?;
            }
            return Ok(());
        }
        if self.credit_inventory_error.is_some() {
            return Ok(());
        }
        let expiry = |credit: &NativeCredit| {
            chrono::DateTime::parse_from_rfc3339(&credit.expires)
                .map(|at| at.with_timezone(&chrono::Utc))
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
        };
        let selected = self
            .credits
            .iter()
            .enumerate()
            .filter(|(_, credit)| credit.available)
            .min_by(|(_, left), (_, right)| {
                expiry(left)
                    .cmp(&expiry(right))
                    .then_with(|| left.id.cmp(&right.id))
            })
            .map(|(credit_index, _)| credit_index);
        if let Some(credit_index) = selected {
            self.prepare(MenuOperation::Redeem(index, credit_index), language)?;
        }
        Ok(())
    }
}
