//! Account-bound reset-credit confirmation recovers the server's immutable pending operation.

use super::Locale;
use super::clean;
use super::input::prompt;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use codex_app_server::account_management::ManagedAccountView;
use std::sync::Arc;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PendingRedemption {
    pub(super) credit_id: String,
    pub(super) operation_id: String,
    pub(super) owner_key: String,
}

pub(super) fn clear_completed(
    pending: &mut std::collections::HashMap<String, PendingRedemption>,
    completed: &PendingRedemption,
) {
    pending.retain(|_, original| original != completed);
}

pub(super) async fn redeem(
    manager: &Arc<AccountManager>,
    account: &ManagedAccountView,
    pending: &mut std::collections::HashMap<String, PendingRedemption>,
    locale: Locale,
) -> anyhow::Result<()> {
    let id = &account.profile_id;
    let result = manager
        .execute(Operation::Credits {
            profile_id: id.clone(),
        })
        .await?;
    if !result.data["inventoryError"].is_null() {
        println!("{}", locale.text("Credit inventory unavailable; current count is unknown. The original reset is retained."));
    }
    let owner = result.data["resetOwnerKey"].as_str().filter(|owner| owner.len() == 64
        && owner.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .ok_or_else(|| anyhow::anyhow!(locale.text("Reset confirmation needs a current account binding. Update the host and reload credits.")))?;
    if !pending.contains_key(id) && !result.data["pendingResetCredit"].is_null() {
        let recovered: codex_app_server_protocol::PendingAccountRateLimitResetCredit =
            serde_json::from_value(result.data["pendingResetCredit"].clone())?;
        anyhow::ensure!(recovered.owner_key == owner, locale.text("The original reset belongs to another account. Return to that account before retrying."));
        let credit_id = recovered
            .credit_id
            .ok_or_else(|| anyhow::anyhow!(locale.text("Credit ID missing")))?;
        anyhow::ensure!(
            !credit_id.is_empty()
                && credit_id.len() <= 256
                && !recovered.idempotency_key.is_empty()
                && recovered.idempotency_key.len() <= 128,
            locale.text("The pending reset could not be verified. Reload credits before retrying.")
        );
        pending.insert(
            id.clone(),
            PendingRedemption {
                credit_id,
                operation_id: recovered.idempotency_key,
                owner_key: owner.into(),
            },
        );
    }
    if let Some(operation) = pending.get(id) {
        anyhow::ensure!(operation.owner_key == owner, locale.text("The original reset belongs to another account. Return to that account before retrying."));
        println!(
            "{}",
            locale.format(
                "A previous reset has an unconfirmed outcome. Credit: {} · Operation: {}",
                &[
                    &clean(&operation.credit_id),
                    &clean(&operation.operation_id)
                ]
            )
        );
        println!("{}", locale.text("RETRY checks the original operation without selecting a new credit. R refreshes quota. Enter returns."));
        match prompt(locale, "Choose R, RETRY, or Enter to return", "")
            .await?
            .as_str()
        {
            "R" | "r" => {
                let result = manager
                    .execute(Operation::Refresh {
                        profile_ids: Some(vec![id.clone()]),
                    })
                    .await?;
                println!("{}", clean(&locale.notice(&result.message)));
                let _ = prompt(locale, "Enter to return", "").await?;
                return Ok(());
            }
            "RETRY" => {}
            _ => return Ok(()),
        }
    } else {
        let credits = result.data["credits"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!(locale.text("Credit list unavailable")))?;
        let mut available = credits
            .iter()
            .filter(|credit| {
                credit["status"].as_str() == Some("available")
                    && credit["resetType"].as_str() == Some("codex_rate_limits")
                    && credit["expiresAt"].as_str().is_none_or(|at| {
                        chrono::DateTime::parse_from_rfc3339(at)
                            .is_ok_and(|at| at > chrono::Utc::now())
                    })
            })
            .collect::<Vec<_>>();
        available.sort_by(|left, right| {
            let expiry = |credit: &serde_json::Value| {
                credit["expiresAt"]
                    .as_str()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| at.with_timezone(&chrono::Utc))
            };
            expiry(left)
                .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
                .cmp(&expiry(right).unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC))
                .then_with(|| left["id"].as_str().cmp(&right["id"].as_str()))
        });
        let Some(credit) = available.first() else {
            println!(
                "{}",
                locale.text(
                    "No available reset credits. Refresh quota or wait for the natural reset."
                )
            );
            let _ = prompt(locale, "Enter to return", "").await?;
            return Ok(());
        };
        println!(
            "{}",
            locale.format(
                "This consumes one reset credit for {}.",
                &[&clean(&account.label)]
            )
        );
        println!(
            "{}",
            locale.format(
                "{} · Expires {}",
                &[
                    &clean(
                        credit["title"]
                            .as_str()
                            .unwrap_or_else(|| locale.text("Reset credit"))
                    ),
                    &clean(
                        credit["expiresAt"]
                            .as_str()
                            .unwrap_or_else(|| locale.text("No expiry reported"))
                    ),
                ]
            )
        );
        println!(
            "{}",
            locale.text("Earliest-expiring available credit selected.")
        );
        if prompt(locale, "Type REDEEM to confirm", "").await? != "REDEEM" {
            return Ok(());
        }
        pending.insert(
            id.clone(),
            PendingRedemption {
                credit_id: credit["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!(locale.text("Credit ID missing")))?
                    .into(),
                operation_id: codex_protocol::ThreadId::new().to_string(),
                owner_key: owner.into(),
            },
        );
    }
    let operation = pending[id].clone();
    println!(
        "{}",
        locale.format("Reset operation: {}", &[&clean(&operation.operation_id)])
    );
    match manager
        .execute(Operation::Redeem {
            profile_id: id.clone(),
            credit_id: operation.credit_id.clone(),
            idempotency_key: operation.operation_id.clone(),
            expected_owner_key: Some(operation.owner_key.clone()),
        })
        .await
    {
        Ok(result) => {
            clear_completed(pending, &operation);
            println!("{}", clean(locale.message(&result.message)));
        }
        Err(error) => println!(
            "{}\n{}",
            clean(locale.message(&error.to_string())),
            locale.text("The operation ID was retained. Refresh quota before retrying.")
        ),
    }
    let _ = prompt(locale, "Enter to return", "").await?;
    Ok(())
}

#[cfg(test)]
#[path = "account_manager_credits_tests.rs"]
mod tests;
