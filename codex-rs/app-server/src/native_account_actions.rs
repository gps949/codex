//! Captured native menu intents with exact-target rechecks and explicit confirmations.

use super::*;
use crate::account_management::AccountManager;
use crate::account_management::AccountManagerOperation;
use crate::account_management::AccountManagerResult;
use crate::account_management::LoginProgress;
use std::sync::Arc;

#[path = "native_account_dialogs.rs"]
mod dialogs;
#[path = "native_account_prepare.rs"]
mod prepare;
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
    notice: String,
    return_page: MenuPage,
    login: Option<LoginProgress>,
    redemption_keys: std::collections::HashMap<(String, String), String>,
    identities: std::collections::HashMap<String, Option<String>>,
}

struct PendingOperation {
    operation: AccountManagerOperation,
    target: Option<FrozenAccount>,
    expected_identity: Option<String>,
    title: String,
    description: String,
    return_page: MenuPage,
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

impl NativeMenuSession {
    pub(crate) fn new(inventory: FrozenAccountInventory) -> Self {
        Self {
            inventory,
            page: MenuPage::Home,
            pending: None,
            credits: vec![],
            credit_target: None,
            credit_identity: None,
            notice: String::new(),
            return_page: MenuPage::Home,
            login: None,
            redemption_keys: std::collections::HashMap::new(),
            identities: std::collections::HashMap::new(),
        }
    }

    pub(crate) async fn handle(
        &mut self,
        answer: MenuAnswer,
        manager: &Arc<AccountManager>,
        language: NativeAccountLanguage,
        context: &crate::account_management::AccountOperationContext,
    ) -> anyhow::Result<bool> {
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
                });
                self.page = MenuPage::Confirm;
            }
            MenuAnswer::Action(MenuAction::Close) => return Ok(false),
            MenuAnswer::Action(MenuAction::Language) => unreachable!("worker handles language"),
            MenuAnswer::Action(MenuAction::Page(page)) => {
                self.pending = None;
                self.page = page;
            }
            MenuAnswer::Action(MenuAction::Prepare(operation)) => {
                self.prepare(operation, language)?
            }
            MenuAnswer::Action(MenuAction::Execute(operation)) => {
                self.execute_read(operation, manager, language).await?;
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
                let result = manager
                    .execute_with_context(pending.operation, context)
                    .await?;
                let login_started =
                    match serde_json::from_value::<LoginProgressWire>(result.data.clone()) {
                        Ok(login) => {
                            self.login = Some(login.into());
                            true
                        }
                        Err(_) => false,
                    };
                self.reload(manager).await?;
                if login_started {
                    self.page = MenuPage::Login;
                    self.update_login(manager).await;
                } else {
                    self.show_result(
                        result,
                        self.return_to_target(pending.target.as_ref(), pending.return_page),
                    );
                }
            }
        }
        Ok(true)
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
    ) -> anyhow::Result<()> {
        if operation == MenuOperation::Reload {
            self.reload(manager).await?;
            self.page = MenuPage::Overview(0);
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
                    .execute(AccountManagerOperation::CancelLogin {
                        operation_id: login.operation_id.clone(),
                    })
                    .await?;
            }
            self.update_login(manager).await;
            self.page = MenuPage::Login;
            return Ok(());
        }
        let (account_operation, target, return_page) = match operation {
            MenuOperation::Refresh(first) => {
                let profiles = self
                    .inventory
                    .accounts
                    .iter()
                    .skip(first)
                    .take(PAGE_SIZE)
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
                    MenuPage::Overview(first / PAGE_SIZE),
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
        let result = manager.execute(account_operation).await?;
        self.reload(manager).await?;
        if let MenuOperation::Credits(index) = operation {
            self.credits = result
                .data
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
                    let not_expired = expires.is_none_or(|expires| {
                        chrono::DateTime::parse_from_rfc3339(expires)
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
            self.credit_identity = credit_identity;
            let index = self
                .credit_target
                .as_ref()
                .and_then(|target| {
                    self.inventory
                        .accounts
                        .iter()
                        .position(|account| account.id == target.id)
                })
                .unwrap_or(index);
            self.page = MenuPage::Credits(index, 0);
        } else {
            self.show_result(result, self.return_to_target(target.as_ref(), return_page));
        }
        Ok(())
    }

    pub(crate) async fn reload(&mut self, manager: &Arc<AccountManager>) -> anyhow::Result<()> {
        self.inventory = FrozenAccountInventory::from_inventory(manager.inventory().await?);
        self.bind_targets(manager);
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
        self.notice = format!(
            "{}\n{}",
            language.text(
                "No success was confirmed. Inspect the result before retrying.",
                "尚未确认成功；请查看结果后再重试。"
            ),
            error
        );
        self.return_page = MenuPage::Home;
        self.page = MenuPage::Result;
    }

    fn return_to_target(&self, target: Option<&FrozenAccount>, fallback: MenuPage) -> MenuPage {
        match target {
            Some(target) => self
                .inventory
                .accounts
                .iter()
                .position(|account| account.id == target.id)
                .map(MenuPage::Detail)
                .unwrap_or(MenuPage::Overview(0)),
            None => fallback,
        }
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
