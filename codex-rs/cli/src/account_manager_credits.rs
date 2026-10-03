//! Reset-credit selection and recovery preserve the original operation identity.

use super::Locale;
use super::clean;
use super::input::prompt;
use codex_app_server::account_management::AccountManager;
use codex_app_server::account_management::AccountManagerOperation as Operation;
use codex_app_server::account_management::ManagedAccountView;
use std::sync::Arc;

/// Retains the exact operation when the backend outcome is not yet confirmed.
pub(super) struct PendingRedemption {
    credit_id: String,
    operation_id: String,
}

pub(super) async fn redeem(
    manager: &Arc<AccountManager>,
    account: &ManagedAccountView,
    pending: &mut std::collections::HashMap<String, PendingRedemption>,
    locale: Locale,
) -> anyhow::Result<()> {
    let id = &account.profile_id;
    if let Some(operation) = pending.get(id) {
        println!(
            "{}",
            locale.format(
                "A previous reset has an unconfirmed outcome. Credit: {} · Operation: {}",
                &[&clean(&operation.credit_id), &operation.operation_id]
            )
        );
        println!("{}", locale.text(
            "R refreshes quota. RETRY uses the same credit and operation ID. REVIEW clears the pending record only after you check its outcome."
        ));
        match prompt(locale, "Choose R, RETRY, REVIEW, or Enter to return", "")
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
            "REVIEW" => {
                let result = manager
                    .execute(Operation::Credits {
                        profile_id: id.clone(),
                    })
                    .await?;
                let credit = result.data["credits"].as_array().and_then(|credits| {
                    credits
                        .iter()
                        .find(|credit| credit["id"].as_str() == Some(operation.credit_id.as_str()))
                });
                println!(
                    "{}",
                    locale.format(
                        "Original credit status: {}",
                        &[locale.message(credit.map_or("No longer listed", |credit| {
                            credit["status"].as_str().unwrap_or("Unknown")
                        }))]
                    )
                );
                println!("{}", locale.text(
                    "Clear only after checking the provider's quota and credit history. This forgets the pending record; it does not undo a redemption."
                ));
                if prompt(
                    locale,
                    "Type REVIEWED to confirm you checked the outcome",
                    "",
                )
                .await?
                    == "REVIEWED"
                {
                    pending.remove(id);
                }
                return Ok(());
            }
            "RETRY" => {}
            _ => return Ok(()),
        }
    } else {
        let result = manager
            .execute(Operation::Credits {
                profile_id: id.clone(),
            })
            .await?;
        let credits = result.data["credits"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!(locale.text("Credit list unavailable")))?;
        let available: Vec<_> = credits
            .iter()
            .filter(|credit| {
                credit["status"].as_str() == Some("available")
                    && credit["resetType"].as_str() == Some("codex_rate_limits")
                    && credit["expiresAt"].as_str().is_none_or(|at| {
                        chrono::DateTime::parse_from_rfc3339(at)
                            .is_ok_and(|at| at > chrono::Utc::now())
                    })
            })
            .collect();
        if available.is_empty() {
            println!(
                "{}",
                locale.text(
                    "No available reset credits. Refresh quota or wait for the natural reset."
                )
            );
            let _ = prompt(locale, "Enter to return", "").await?;
            return Ok(());
        }
        for (index, credit) in available.iter().enumerate() {
            println!(
                "{}",
                locale.format(
                    "{}: {} · {} · Expires {}",
                    &[
                        &(index + 1).to_string(),
                        &clean(
                            credit["title"]
                                .as_str()
                                .unwrap_or_else(|| locale.text("Reset credit"))
                        ),
                        &clean(match credit["resetType"].as_str() {
                            Some("codex_rate_limits") if locale == Locale::SimplifiedChinese =>
                                locale.text("Codex quota"),
                            Some(scope) => scope,
                            None => locale.text("Unknown scope"),
                        }),
                        &clean(
                            credit["expiresAt"]
                                .as_str()
                                .unwrap_or_else(|| locale.text("No expiry reported"))
                        )
                    ]
                )
            );
        }
        let choice = prompt(locale, "Credit number (Enter returns)", "").await?;
        if choice.is_empty() {
            return Ok(());
        }
        let credit = choice
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| available.get(index))
            .ok_or_else(|| anyhow::anyhow!(locale.text("Choose a listed credit number")))?;
        println!(
            "{}",
            locale.format(
                "This consumes one reset credit for {}.",
                &[&clean(&account.label)]
            )
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
                operation_id: format!(
                    "manager-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos()
                ),
            },
        );
    }
    let operation = &pending[id];
    // Show a recoverable identifier before contacting the backend, even if input closes.
    println!(
        "{}",
        locale.format("Reset operation: {}", &[&operation.operation_id])
    );
    match manager
        .execute(Operation::Redeem {
            profile_id: id.clone(),
            credit_id: operation.credit_id.clone(),
            idempotency_key: operation.operation_id.clone(),
        })
        .await
    {
        Ok(result) => {
            pending.remove(id);
            println!("{}", clean(locale.message(&result.message)));
        }
        Err(error) => {
            println!(
                "{}\n{}",
                clean(locale.message(&error.to_string())),
                locale.text("The operation ID was retained. Refresh quota before retrying.")
            );
        }
    }
    let _ = prompt(locale, "Enter to return", "").await?;
    Ok(())
}
