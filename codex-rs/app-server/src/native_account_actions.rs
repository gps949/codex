//! Captured native menu intents with exact-target rechecks and explicit confirmations.

use super::*;
use crate::account_management::AccountManager;
use crate::account_management::AccountManagerOperation;
use crate::account_management::AccountManagerResult;
use crate::account_management::LoginProgress;
use std::sync::Arc;

#[path = "native_account_credits.rs"]
mod credit_list;
#[path = "native_account_dialogs.rs"]
mod dialogs;
#[path = "native_account_location.rs"]
mod location;
#[path = "native_account_prepare.rs"]
mod prepare;
#[path = "native_account_quick_actions.rs"]
mod quick_actions;
#[path = "native_account_settings.rs"]
mod settings;
#[path = "native_account_targets.rs"]
mod targets;

use settings::setting_change;
use targets::same_target;
use targets::validate_settings;
use targets::validate_target;

#[cfg(test)]
#[path = "native_account_actions_tests.rs"]
mod tests;

pub(crate) struct NativeMenuSession {
    pub(crate) inventory: FrozenAccountInventory,
    pub(crate) page: MenuPage,
    pending: Option<PendingOperation>,
    credits: Vec<NativeCredit>,
    credit_target: Option<FrozenAccount>,
    credit_identity: Option<String>,
    credit_owner_key: Option<String>,
    credit_inventory_error: Option<String>,
    pending_reset: Option<NativePendingReset>,
    notice: String,
    confirmed_reset_receipt: Option<String>,
    return_page: MenuPage,
    login: Option<LoginProgress>,
    redemption_keys: std::collections::HashMap<(String, String), String>,
    identities: std::collections::HashMap<String, Option<String>>,
    list_origin: MenuOrigin,
}

struct PendingOperation {
    operation: AccountManagerOperation,
    target: Option<FrozenAccount>,
    expected_identity: Option<String>,
    title: String,
    description: String,
    return_page: MenuPage,
    pending_reset: Option<NativePendingReset>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativePendingReset {
    owner_key: String,
    idempotency_key: String,
    credit_id: Option<String>,
}

struct NativeCredit {
    id: String,
    expires: String,
    available: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MenuAnswer {
    Action(MenuAction),
    Text(String),
}

pub(crate) enum MenuOutcome {
    Continue,
    Close,
    Completed(String),
}

impl NativeMenuSession {
    pub(crate) fn new(inventory: FrozenAccountInventory, entry: NativeMenuEntry) -> Self {
        Self {
            inventory,
            page: match entry {
                NativeMenuEntry::Quick => MenuPage::Quick(0),
                NativeMenuEntry::Manage => MenuPage::Home,
            },
            pending: None,
            credits: vec![],
            credit_target: None,
            credit_identity: None,
            credit_owner_key: None,
            credit_inventory_error: None,
            pending_reset: None,
            notice: String::new(),
            confirmed_reset_receipt: None,
            return_page: MenuPage::Home,
            login: None,
            redemption_keys: std::collections::HashMap::new(),
            identities: std::collections::HashMap::new(),
            list_origin: match entry {
                NativeMenuEntry::Quick => MenuOrigin::Quick(0),
                NativeMenuEntry::Manage => MenuOrigin::Overview(0),
            },
        }
    }

    pub(crate) async fn handle(
        &mut self,
        answer: MenuAnswer,
        manager: &Arc<AccountManager>,
        language: NativeAccountLanguage,
        context: &crate::account_management::AccountOperationContext,
    ) -> anyhow::Result<MenuOutcome> {
        self.confirmed_reset_receipt = None;
        match answer {
            MenuAnswer::Text(label) => {
                let MenuPage::Rename(index) = self.page else {
                    anyhow::bail!("This page does not accept text");
                };
                let target = self.account(index)?.clone();
                self.pending = Some(PendingOperation {
                    operation: match &target.detail {
                        AccountDetail::Subscription { .. } => AccountManagerOperation::Update {
                            profile_id: target.id.clone(),
                            label: Some(label.clone()),
                            priority: None,
                            disabled: None,
                        },
                        AccountDetail::Api { account, .. } => {
                            let mut account = account.clone();
                            account.label = label.clone();
                            AccountManagerOperation::ApiUpdate { account }
                        }
                    },
                    title: language.text("Save account name?", "保存账号名称？").into(),
                    description: format!(
                        "{}\n{}: {label}",
                        confirmation_target(&target, index),
                        language.text("New name", "新名称")
                    ),
                    expected_identity: self.identities.get(&target.id).cloned().flatten(),
                    target: Some(target),
                    return_page: MenuPage::Detail(index),
                    pending_reset: None,
                });
                self.return_page = MenuPage::Detail(index);
                self.page = MenuPage::Confirm;
            }
            MenuAnswer::Action(MenuAction::Close) => return Ok(MenuOutcome::Close),
            MenuAnswer::Action(MenuAction::Language) => unreachable!("worker handles language"),
            MenuAnswer::Action(MenuAction::Page(page)) => {
                self.pending = None;
                self.page = page;
                match page {
                    MenuPage::Quick(page) | MenuPage::QuickOptions(page) => {
                        self.list_origin = MenuOrigin::Quick(page)
                    }
                    MenuPage::Overview(page) | MenuPage::Browse(page) => {
                        self.list_origin = MenuOrigin::Overview(page)
                    }
                    MenuPage::Home => self.list_origin = MenuOrigin::Overview(0),
                    _ => {}
                }
            }
            MenuAnswer::Action(MenuAction::Prepare(operation)) => {
                self.prepare(operation, language)?
            }
            MenuAnswer::Action(MenuAction::Execute(operation)) => {
                self.execute_read(operation, manager, language, context)
                    .await?;
            }
            MenuAnswer::Action(
                action @ (MenuAction::SelectSubscription(_)
                | MenuAction::Automatic
                | MenuAction::SetStrategy(_)),
            ) => {
                return self.execute_quick(action, manager, language, context).await;
            }
            MenuAnswer::Action(MenuAction::Apply) => {
                let pending = self
                    .pending
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("Confirmation is no longer active"))?;
                let fresh = FrozenAccountInventory::from_inventory(manager.inventory().await?);
                if let Some(target) = &pending.target {
                    validate_target(target, &fresh)?;
                    self.validate_identity(target, pending.expected_identity.as_deref(), manager)?;
                }
                validate_settings(&pending.operation, &self.inventory, &fresh)?;
                let return_location =
                    location::MenuLocation::capture(pending.return_page, &self.inventory);
                if let Some(expected) = &pending.pending_reset {
                    let target = pending
                        .target
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Previous reset owner is unavailable"))?;
                    let credits = manager
                        .execute_with_context(
                            AccountManagerOperation::Credits {
                                profile_id: target.id.clone(),
                            },
                            context,
                        )
                        .await?;
                    let actual: Option<NativePendingReset> = serde_json::from_value(
                        credits
                            .data
                            .get("pendingResetCredit")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    )?;
                    // A terminal original operation has no pending record; reuse its exact key.
                    anyhow::ensure!(
                        actual.as_ref().is_none_or(|actual| actual == expected),
                        "Previous reset operation changed. Reload its credits before retrying."
                    );
                    self.validate_identity(target, pending.expected_identity.as_deref(), manager)?;
                }
                let redemption = match &pending.operation {
                    AccountManagerOperation::Redeem {
                        expected_owner_key: Some(owner_key),
                        credit_id,
                        idempotency_key,
                        ..
                    } => Some(NativePendingReset {
                        owner_key: owner_key.clone(),
                        credit_id: Some(credit_id.clone()),
                        idempotency_key: idempotency_key.clone(),
                    }),
                    _ => None,
                };
                let result = manager
                    .execute_with_context(pending.operation, context)
                    .await?;
                if let Some(redemption) = &redemption {
                    self.confirmed_reset_receipt = Some(result.message.clone());
                    self.return_page = return_location.restore(&self.inventory);
                    if self.pending_reset.as_ref() == Some(redemption) {
                        self.pending_reset = None;
                    }
                    if let Some(credit_id) = &redemption.credit_id {
                        let tuple = (redemption.owner_key.clone(), credit_id.clone());
                        if self.redemption_keys.get(&tuple) == Some(&redemption.idempotency_key) {
                            self.redemption_keys.remove(&tuple);
                        }
                    }
                }
                let login_started =
                    match serde_json::from_value::<LoginProgressWire>(result.data.clone()) {
                        Ok(login) => {
                            self.login = Some(login.into());
                            true
                        }
                        Err(_) => false,
                    };
                // Post-reset reads cannot turn a confirmed backend result into an error receipt.
                let reloaded = self.reload(manager).await;
                if redemption.is_none() {
                    reloaded?;
                }
                let mut return_page = return_location.restore(&self.inventory);
                if let Some(redemption) = &redemption {
                    let mut credits_reloaded = false;
                    if self.credit_owner_key.as_ref() == Some(&redemption.owner_key)
                        && self.pending_reset.is_none()
                    {
                        self.credits.clear();
                        let refresh = tokio::time::timeout(
                            std::time::Duration::from_secs(/*secs*/ 1),
                            async {
                                let target = pending.target.as_ref().ok_or_else(|| {
                                    anyhow::anyhow!("The completed reset account is unavailable")
                                })?;
                                self.validate_identity(
                                    target,
                                    pending.expected_identity.as_deref(),
                                    manager,
                                )?;
                                let credits = manager
                                    .execute_with_context(
                                        AccountManagerOperation::Credits {
                                            profile_id: target.id.clone(),
                                        },
                                        context,
                                    )
                                    .await?;
                                anyhow::ensure!(
                                    credits
                                        .data
                                        .get("resetOwnerKey")
                                        .and_then(serde_json::Value::as_str)
                                        == Some(redemption.owner_key.as_str()),
                                    "Account changed after the reset completed; reload its credits"
                                );
                                self.load_credits(
                                    credits.data,
                                    pending.target.clone(),
                                    pending.expected_identity.clone(),
                                    language,
                                )
                            },
                        )
                        .await;
                        match refresh {
                            Ok(Ok(())) => credits_reloaded = true,
                            Ok(Err(error)) => {
                                self.credit_inventory_error =
                                    Some(bounded_text(&error.to_string(), /*max_chars*/ 240))
                            }
                            Err(error) => {
                                self.credit_inventory_error =
                                    Some(bounded_text(&error.to_string(), /*max_chars*/ 240))
                            }
                        }
                    }
                    if !credits_reloaded && let MenuPage::Credits(index, _) = return_page {
                        return_page = MenuPage::Detail(index);
                    }
                }
                if login_started {
                    self.page = MenuPage::Login;
                    self.update_login(manager).await;
                } else {
                    self.show_result(result, return_page);
                }
            }
        }
        Ok(MenuOutcome::Continue)
    }

    fn account(&self, index: usize) -> anyhow::Result<&FrozenAccount> {
        self.inventory
            .accounts
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("Account no longer appears in this menu"))
    }

    async fn execute_read(
        &mut self,
        operation: MenuOperation,
        manager: &Arc<AccountManager>,
        language: NativeAccountLanguage,
        context: &crate::account_management::AccountOperationContext,
    ) -> anyhow::Result<()> {
        if operation == MenuOperation::Reload {
            self.reload(manager).await?;
            return Ok(());
        }
        if matches!(
            operation,
            MenuOperation::LoginCheck | MenuOperation::LoginCancel
        ) {
            if operation == MenuOperation::LoginCancel
                && let Some(login) = &self.login
            {
                manager
                    .execute_with_context(
                        AccountManagerOperation::CancelLogin {
                            operation_id: login.operation_id.clone(),
                        },
                        context,
                    )
                    .await?;
            }
            self.update_login(manager).await;
            if self.login.as_ref().is_some_and(|login| {
                matches!(login.status.as_str(), "completed" | "cancelled" | "failed")
            }) {
                self.reload(manager).await?;
            }
            self.page = MenuPage::Login;
            return Ok(());
        }
        let (account_operation, target, return_page) = match operation {
            MenuOperation::Refresh(first) | MenuOperation::RefreshQuick(first) => {
                let (indices, return_page) = if matches!(operation, MenuOperation::RefreshQuick(_))
                {
                    (
                        self.inventory
                            .quick_indices()
                            .into_iter()
                            .skip(first * QUICK_PAGE_SIZE)
                            .take(QUICK_PAGE_SIZE)
                            .collect::<Vec<_>>(),
                        MenuPage::Quick(first),
                    )
                } else {
                    (
                        (first..self.inventory.accounts.len().min(first + PAGE_SIZE)).collect(),
                        match self.page {
                            MenuPage::Refresh(_) => MenuPage::Refresh(first),
                            _ => MenuPage::Overview(first / PAGE_SIZE),
                        },
                    )
                };
                let profiles = indices
                    .into_iter()
                    .map(|index| &self.inventory.accounts[index])
                    .filter(|account| {
                        !account.disabled
                            && account.login_state == "signedIn"
                            && matches!(account.detail, AccountDetail::Subscription { .. })
                    })
                    .map(|account| account.id.clone())
                    .collect();
                (
                    AccountManagerOperation::Refresh {
                        profile_ids: Some(profiles),
                    },
                    None,
                    return_page,
                )
            }
            MenuOperation::RefreshAccount(index) | MenuOperation::Credits(index) => {
                let account = self.account(index)?.clone();
                anyhow::ensure!(
                    !account.disabled && account.login_state == "signedIn",
                    "Complete account login and enable this account before contacting the backend"
                );
                let operation = if matches!(operation, MenuOperation::Credits(_)) {
                    AccountManagerOperation::Credits {
                        profile_id: account.id.clone(),
                    }
                } else {
                    AccountManagerOperation::Refresh {
                        profile_ids: Some(vec![account.id.clone()]),
                    }
                };
                (operation, Some(account), MenuPage::Detail(index))
            }
            _ => anyhow::bail!("This operation needs a confirmation"),
        };
        let fresh = FrozenAccountInventory::from_inventory(manager.inventory().await?);
        if let Some(target) = &target {
            validate_target(target, &fresh)?;
            self.validate_identity(
                target,
                self.identities.get(&target.id).and_then(Option::as_deref),
                manager,
            )?;
        }
        if let AccountManagerOperation::Refresh {
            profile_ids: Some(ids),
        } = &account_operation
        {
            for id in ids {
                let target = self
                    .inventory
                    .accounts
                    .iter()
                    .find(|target| &target.id == id)
                    .ok_or_else(|| anyhow::anyhow!("Refresh target no longer appears"))?;
                validate_target(target, &fresh)?;
                self.validate_identity(
                    target,
                    self.identities.get(id).and_then(Option::as_deref),
                    manager,
                )?;
            }
        }
        let credit_identity = target
            .as_ref()
            .and_then(|target| self.identities.get(&target.id).cloned().flatten());
        let return_location = location::MenuLocation::capture(return_page, &self.inventory);
        context.ensure_current().await?;
        let result = manager
            .execute_with_context(account_operation, context)
            .await?;
        self.reload(manager).await?;
        if matches!(operation, MenuOperation::Credits(_)) {
            self.load_credits(result.data, target, credit_identity, language)?;
        } else {
            self.page = return_location.restore(&self.inventory);
        }
        Ok(())
    }

    pub(crate) async fn reload(&mut self, manager: &Arc<AccountManager>) -> anyhow::Result<()> {
        let page = location::MenuLocation::capture(self.page, &self.inventory);
        let return_page = location::MenuLocation::capture(self.return_page, &self.inventory);
        let origin = location::MenuLocation::capture(self.list_origin.page(), &self.inventory);
        self.inventory = FrozenAccountInventory::from_inventory(manager.inventory().await?);
        self.bind_targets(manager);
        self.page = page.restore(&self.inventory);
        self.return_page = return_page.restore(&self.inventory);
        self.list_origin = match origin.restore(&self.inventory) {
            MenuPage::Quick(page) => MenuOrigin::Quick(page),
            MenuPage::Overview(page) => MenuOrigin::Overview(page),
            _ => self.list_origin,
        };
        Ok(())
    }

    async fn update_login(&mut self, manager: &Arc<AccountManager>) {
        if let Some(login) = &self.login
            && let Some(current) = manager
                .login_progress()
                .await
                .into_iter()
                .find(|progress| progress.operation_id == login.operation_id)
        {
            self.login = Some(current);
        }
    }

    pub(crate) async fn close(&self, manager: &Arc<AccountManager>) {
        if let Some(login) = &self.login
            && login.status == "waiting"
        {
            let _ = manager
                .execute(AccountManagerOperation::CancelLogin {
                    operation_id: login.operation_id.clone(),
                })
                .await;
        }
    }

    pub(crate) fn error(&mut self, error: anyhow::Error, language: NativeAccountLanguage) {
        if let Some(receipt) = self.confirmed_reset_receipt.take() {
            self.notice = receipt;
            if let MenuPage::Credits(index, _) = self.return_page {
                self.return_page = MenuPage::Detail(index);
            }
            self.page = MenuPage::Result;
            return;
        }
        self.notice = format!(
            "{}\n{}",
            language.text(
                "No success was confirmed. Inspect the result before retrying.",
                "尚未确认成功；请查看结果后再重试。"
            ),
            error
        );
        self.return_page = match self.page {
            MenuPage::Confirm | MenuPage::Result => self.return_page,
            page => page,
        };
        self.page = MenuPage::Result;
    }

    fn show_result(&mut self, result: AccountManagerResult, return_page: MenuPage) {
        self.notice = result.message;
        self.return_page = return_page;
        self.page = MenuPage::Result;
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginProgressWire {
    operation_id: String,
    profile_id: Option<String>,
    verification_url: Option<String>,
    user_code: Option<String>,
    status: String,
    message: String,
}
impl From<LoginProgressWire> for LoginProgress {
    fn from(progress: LoginProgressWire) -> Self {
        Self {
            operation_id: progress.operation_id,
            profile_id: progress.profile_id,
            verification_url: progress.verification_url,
            user_code: progress.user_code,
            status: progress.status,
            message: progress.message,
        }
    }
}
