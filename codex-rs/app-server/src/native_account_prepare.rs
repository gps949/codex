//! Builds explicit confirmation intent without performing account mutations.

use super::*;

impl NativeMenuSession {
    pub(in crate::native_account_view) fn prepare(
        &mut self,
        operation: MenuOperation,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<()> {
        if let MenuOperation::Relogin(index) = operation
            && let Some(login) = &self.login
            && login.status == "waiting"
            && login.profile_id.as_deref() == Some(self.account(index)?.id.as_str())
        {
            self.page = MenuPage::Login;
            return Ok(());
        }
        let mut target = None;
        let mut return_page = MenuPage::Home;
        let (title, description, operation) = match operation {
            MenuOperation::Automatic => (
                language.text("Use subscription pool?", "使用订阅账号池？"),
                language
                    .text(
                        "Exit manual API use. No cooldown is cleared and no credit is used.",
                        "退出手动 API 使用；不会清除冷却或使用重置券。",
                    )
                    .into(),
                AccountManagerOperation::Automatic,
            ),
            MenuOperation::PrimaryRoot => (
                language.text("Use root host login?", "主登录改用根登录？"),
                language
                    .text(
                        "Remote owner may change. Inference selection is unchanged.",
                        "Remote 身份可能改变；推理选择不变。",
                    )
                    .into(),
                AccountManagerOperation::PrimaryRoot,
            ),
            MenuOperation::PrimaryLogout => (
                language.text("Sign out host?", "退出主登录？"),
                language
                    .text(
                        "Remote disconnects. Subscription and API credentials are retained.",
                        "Remote 将断开；保留订阅与 API 凭据。",
                    )
                    .into(),
                AccountManagerOperation::PrimaryLogout,
            ),
            MenuOperation::Add => {
                anyhow::ensure!(
                    self.login
                        .as_ref()
                        .is_none_or(|login| login.status != "waiting"),
                    "Complete or cancel this menu's existing sign-in first"
                );
                (language.text("Add subscription login?", "添加订阅账号？"), language.text("Verify the desired account in a browser. Closing this menu cancels its pending sign-in.", "在浏览器验证目标账号；关闭菜单将取消待完成登录。").into(), AccountManagerOperation::Login { profile_id: None, label: None })
            }
            MenuOperation::FallbackOff => {
                let mut config = self.inventory.fallback.clone();
                config.enabled = false;
                (
                    language.text("Disable paid fallback?", "关闭付费兜底？"),
                    language
                        .text(
                            "API targets remain available for explicit manual use.",
                            "API 目标仍可明确手动选择。",
                        )
                        .into(),
                    AccountManagerOperation::ApiFallback { config },
                )
            }
            MenuOperation::Setting(index) => {
                let (key, value, description) =
                    setting_change(&self.inventory.settings, index, language)?;
                return_page = MenuPage::Settings(index / 3);
                (
                    language.text("Change pool setting?", "修改账号池设置？"),
                    description,
                    AccountManagerOperation::Settings {
                        values: serde_json::json!({key: value}),
                    },
                )
            }
            MenuOperation::Use(index)
            | MenuOperation::Retry(index)
            | MenuOperation::Disable(index)
            | MenuOperation::Enable(index)
            | MenuOperation::ClearLabel(index)
            | MenuOperation::RemoveKeep(index)
            | MenuOperation::RemoveDelete(index)
            | MenuOperation::Relogin(index)
            | MenuOperation::PrimaryUse(index)
            | MenuOperation::Redeem(index, _)
            | MenuOperation::ApiUse(index)
            | MenuOperation::ApiRemove(index)
            | MenuOperation::ApiFallback(index) => {
                let account = self.account(index)?.clone();
                if matches!(
                    operation,
                    MenuOperation::ApiUse(_) | MenuOperation::ApiFallback(_)
                ) {
                    anyhow::ensure!(
                        !account.disabled
                            && matches!(account.detail, AccountDetail::Api { has_key: true, .. }),
                        "Enable this API target and configure its key before paid selection or fallback"
                    );
                }
                return_page = MenuPage::Detail(index);
                let id = account.id.clone();
                let (title, explanation, operation) = match operation {
                    MenuOperation::Use(_) => (language.text("Use this account?", "使用此账号？"), language.text("Subsequent requests use this account; automatic failover remains enabled.", "后续请求使用此账号；仍可自动切换。").into(), AccountManagerOperation::Use { profile_id: id }),
                    MenuOperation::Retry(_) => (language.text("Probe external reset?", "尝试外部重置恢复？"), language.text("Clear local quota cooldown for one probe. No reset credit is redeemed.", "清除本地额度冷却以尝试一次；不会兑换重置券。").into(), AccountManagerOperation::Retry { profile_id: id }),
                    MenuOperation::Disable(_) | MenuOperation::Enable(_) | MenuOperation::ClearLabel(_) => {
                        let update = match &account.detail {
                            AccountDetail::Subscription { .. } => AccountManagerOperation::Update { profile_id: id,
                                label: matches!(operation, MenuOperation::ClearLabel(_)).then(String::new), priority: None,
                                disabled: match operation { MenuOperation::Disable(_) => Some(true), MenuOperation::Enable(_) => Some(false), _ => None } },
                            AccountDetail::Api { account, .. } => { let mut account = account.clone(); account.disabled = matches!(operation, MenuOperation::Disable(_)); AccountManagerOperation::ApiUpdate { account } }
                        };
                        (language.text("Update account?", "修改账号？"), language.text("Only this account is changed. Host sign-in is unchanged.", "仅修改此账号；主登录不变。").into(), update)
                    },
                    MenuOperation::RemoveKeep(_) | MenuOperation::RemoveDelete(_) => (language.text("Remove this account?", "移除此账号？"), if id == "legacy-root" {
                        language.text("Remove pool membership and retain root sign-in credentials.", "移除池成员并保留根登录凭据。") } else if matches!(operation, MenuOperation::RemoveKeep(_)) {
                        language.text("Remove membership and keep local credentials.", "移除池成员，保留本地凭据。") } else { language.text("Remove membership and delete managed credentials. Adding it again requires sign-in.", "移除池成员并删除受管凭据；再次添加需要登录。") }.into(), AccountManagerOperation::Remove { keep_credentials: id == "legacy-root" || matches!(operation, MenuOperation::RemoveKeep(_)), profile_id: id }),
                    MenuOperation::Relogin(_) => { anyhow::ensure!(self.login.as_ref().is_none_or(|login| login.status != "waiting"), "Complete or cancel this menu's existing sign-in first"); (language.text("Sign in again?", "重新登录？"), language.text("Verify this account in a browser. Existing credentials are retained until verification succeeds.", "在浏览器验证此账号；验证成功前保留现有凭据。").into(), AccountManagerOperation::Login { profile_id: Some(id), label: None }) },
                    MenuOperation::PrimaryUse(_) => (language.text("Change host sign-in?", "更换主登录账号？"), language.text("Remote owner changes; pairing may be needed. Inference selection stays unchanged.", "更换 Remote 身份，可能需要配对；推理选择不变。").into(), AccountManagerOperation::PrimaryUse { profile_id: id }),
                    MenuOperation::Redeem(_, credit_index) => {
                        anyhow::ensure!(self.credit_target.as_ref().is_some_and(|target| same_target(target, &account)) && self.credit_identity == self.identities.get(&account.id).cloned().flatten(), "Reload credits for this account before redemption");
                        let credit = self.credits.get(credit_index).ok_or_else(|| anyhow::anyhow!("Credit no longer appears"))?;
                        anyhow::ensure!(credit.available, "This credit is unavailable or has a different quota scope");
                        (language.text("Use one reset credit?", "使用一张重置券？"), format!("{}\n{}: {}\n{}", language.text("One credit is consumed for this account only", "仅为此账号消耗一张券"), language.text("Expires", "到期"), credit.expires,
                            language.text("Backend identity, availability and expiry are checked again before redemption.", "兑换前将再次核对后端身份、券状态与到期时间。")), AccountManagerOperation::Redeem { profile_id: id, credit_id: credit.id.clone(), idempotency_key: self.redemption_keys.entry((account.id.clone(), credit.id.clone())).or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone() })
                    }
                    MenuOperation::ApiUse(_) => (language.text("Select paid API?", "选择付费 API？"), language.text("Conversation content goes to this provider; charges can apply and there is no hard spending cap.\nSubsequent requests use this provider. Use automatic selection for subscriptions.", "会向此提供商发送会话内容；可能收费，没有消费金额硬上限。\n后续请求使用此提供商；自动选择可恢复使用订阅。").into(), AccountManagerOperation::ApiUse { profile_id: id }),
                    MenuOperation::ApiRemove(_) => (language.text("Remove API and key?", "移除 API 与密钥？"), language.text("Delete this target and its locally stored key.", "删除此目标及其本地保存的密钥。").into(), AccountManagerOperation::ApiRemove { profile_id: id }),
                    MenuOperation::ApiFallback(_) => (language.text("Enable paid fallback?", "启用付费兜底？"), language.text("Conversation content goes to this provider; charges can apply and there is no hard spending cap.\nPaid use may start after subscription exhaustion and a 5-minute wait.", "会向此提供商发送会话内容；可能收费，没有消费金额硬上限。\n订阅耗尽并等待 5 分钟后，可能开始付费使用。").into(), AccountManagerOperation::ApiFallback { config: codex_login::ApiAccountFallback { enabled: true, profile_id: Some(id), wait_minutes: 5 } }),
                    _ => unreachable!("matched target operation"),
                };
                let description = if matches!(
                    operation,
                    AccountManagerOperation::ApiUse { .. }
                        | AccountManagerOperation::ApiFallback { .. }
                ) {
                    let AccountDetail::Api { account: api, .. } = &account.detail else {
                        unreachable!("validated API target");
                    };
                    format!(
                        "{explanation}\n{}\nURL: {}\n{}: {}",
                        confirmation_target(&account, index),
                        bounded_text(&api.base_url, /*max_chars*/ 120),
                        language.text("Model", "模型"),
                        bounded_text(&api.model, /*max_chars*/ 96)
                    )
                } else {
                    format!("{}\n{explanation}", confirmation_target(&account, index))
                };
                target = Some(account);
                (title, description, operation)
            }
            MenuOperation::Reload
            | MenuOperation::Refresh(_)
            | MenuOperation::RefreshAccount(_)
            | MenuOperation::Credits(_)
            | MenuOperation::LoginCheck
            | MenuOperation::LoginCancel => {
                anyhow::bail!("This operation does not use a confirmation page")
            }
        };
        let expected_identity = target
            .as_ref()
            .and_then(|target| self.identities.get(&target.id).cloned().flatten());
        self.pending = Some(PendingOperation {
            operation,
            target,
            expected_identity,
            title: title.into(),
            description,
            return_page,
        });
        self.page = MenuPage::Confirm;
        Ok(())
    }
}
