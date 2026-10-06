//! Account-detail and explicit-action pages keep substantive text in descriptions.

use super::*;

impl FrozenAccountInventory {
    pub(super) fn account_page(
        &self,
        page: MenuPage,
        index: usize,
        language: NativeAccountLanguage,
        now: i64,
    ) -> MenuQuestion {
        let nav = |en, zh, description: String, page| {
            choice(language.text(en, zh), description, MenuAction::Page(page))
        };
        let action = |en, zh, description: &str, operation| {
            choice(
                language.text(en, zh),
                description,
                MenuAction::Prepare(operation),
            )
        };
        let read = |en, zh, description: &str, operation| {
            choice(
                language.text(en, zh),
                description,
                MenuAction::Execute(operation),
            )
        };
        let back = |page| nav("Back", "返回", String::new(), page);
        let close = || choice(language.text("Close", "关闭"), "", MenuAction::Close);
        let Some(account) = self.accounts.get(index) else {
            return self.question_at(MenuPage::Home, language, now);
        };
        let account_name = bounded_text(&account.label, /*max_chars*/ 38);
        match page {
                    MenuPage::Detail(_) => {
                        let mut choices = vec![nav("Quota and identity", "额度与身份", self.account_summary(account, language, now), MenuPage::Usage(index))];
                        if matches!(account.detail, AccountDetail::Subscription { .. }) && !account.disabled && account.login_state == "signedIn" {
                            choices.push(read("Use reset credit", "使用重置券", language.text("Select the earliest-expiring available credit; confirm before use", "自动选择最早到期的有效券；使用前需要确认"), MenuOperation::UseResetCredit(index)));
                        }
                        choices.push(nav("Account actions", "账号操作", language.text("Select, refresh, edit or manage login", "切换、刷新、编辑与管理登录").into(), MenuPage::Actions(index)));
                        if choices.len() == 2 {
                            choices.push(nav("Next account", "下一个账号", "".into(), MenuPage::Detail((index + 1) % self.accounts.len())));
                        }
                        choices.extend([back(MenuPage::Overview(index / PAGE_SIZE)), close()]);
                        MenuQuestion::new(account_name, choices)
                    },
                    MenuPage::Usage(_) => {
                        let mut choices = Vec::new();
                        match &account.detail {
                            AccountDetail::Subscription { plan, email, primary, secondary, primary_observed_at, secondary_observed_at, credits, .. } => {
                                for (en, zh, window, observed) in [("Primary quota", "主额度", primary.as_ref(), *primary_observed_at), ("Secondary quota", "次额度", secondary.as_ref(), *secondary_observed_at)] {
                                    let mut detail = window_quota(language.text("Used", "已用"), window, now, language);
                                    if let Some(reset) = window.and_then(|window| window.resets_at) { detail.push_str(&format!("\n{}: {}", language.text("Reset", "重置"), timestamp(reset))); }
                                    detail.push_str(&format!("\n{}", observation_age(observed, now, language)));
                                    choices.push(nav(en, zh, detail, page));
                                }
                                choices.push(nav("Identity", "账号身份", format!("{plan}\n{}\nID: {}\n{}: {}", email.as_deref().unwrap_or("Unknown"), account.id,
                                    language.text("Reset credits (cached)", "重置券（缓存）"), credits.map(|value| value.to_string()).unwrap_or_else(|| "?".into())), page));
                                if let Some(status) = quota_details::status_description(account, language, now) {
                                    choices.push(nav("Quota check and status", "查询结果与状态", status, page));
                                }
                            }
                            AccountDetail::Api { account, has_key, .. } => choices.push(nav("API details", "API 详情", format!("{}\n{}\n{}\n{}", account.model, account.base_url,
                                if *has_key { language.text("Key configured", "已配置密钥") } else { language.text("Key missing", "缺少密钥") }, language.text("Provider charges apply; no subscription quota", "使用由提供商计费，不适用订阅额度")), page)),
                        }
                        choices.push(back(MenuPage::Detail(index)));
                        MenuQuestion::new(language.text("Quota and identity", "额度与身份"), choices)
                    }
                    MenuPage::Actions(_) => {
                        let mut choices = Vec::new();
                        match &account.detail {
                            AccountDetail::Subscription { .. } => {
                                let signed_in = account.login_state == "signedIn";
                                if !signed_in { choices.push(action("Finish sign-in", "完成登录", language.text("Complete browser verification before checking quota or credits", "完成浏览器验证后才能查询额度或重置券"), MenuOperation::Relogin(index))); }
                                if !account.disabled && signed_in && matches!(account.state.as_str(), "ready" | "paused") { choices.push(choice(language.text("Use this account", "使用此账号"), language.text("Applies to subsequent inference requests; failover stays enabled", "应用于后续推理请求；仍可自动切换"), MenuAction::SelectSubscription(index))); }
                                if !account.disabled && signed_in { choices.push(read("Refresh quota", "刷新额度", language.text("Check backend quota; no generating request or credit redemption", "查询后端额度；不会发起推理或兑换券"), MenuOperation::RefreshAccount(index))); }
                                if !account.disabled && signed_in && account.state == "coolingDown" { choices.push(action("Retry after external reset", "外部重置后重试", language.text("Clear local cooldown for one probe; no credit is used", "清除本地冷却以尝试一次；不会用券"), MenuOperation::Retry(index))); }
                                if !account.disabled && signed_in { choices.push(read("Reset credits", "重置券", language.text("Load available credits; using one needs a separate confirmation", "查看可用券；使用需单独确认"), MenuOperation::Credits(index))); }
                                choices.push(nav("More actions", "更多操作", "".into(), MenuPage::More(index)));
                            }
                            AccountDetail::Api { has_key, credential_revision, .. } => {
                                if !account.disabled && *has_key && credential_revision.is_some() {
                                    choices.push(action("Use paid API", "使用付费 API", language.text("Select this provider manually; subsequent usage may be billed", "手动选择此提供商；后续使用可能产生费用"), MenuOperation::ApiUse(index)));
                                    choices.push(action("Enable paid fallback", "启用付费兜底", language.text("Opt in for this provider after subscription exhaustion", "明确允许订阅耗尽后使用此提供商"), MenuOperation::ApiFallback(index)));
                                }
                                let guidance = if account.disabled { language.text("Enable this target before paid selection or fallback", "启用此目标后才可付费选择或兜底") }
                                    else if !*has_key { language.text("Add or replace its key in the host account manager first", "请先在主机账号管理器中添加或更换密钥") } else { "" };
                                choices.push(nav("Edit / remove", "编辑 / 移除", guidance.into(), MenuPage::Membership(index)));
                            }
                        }
                        choices.push(back(MenuPage::Detail(index)));
                        MenuQuestion::new(language.text("Account actions", "账号操作"), choices)
                    }
                    MenuPage::More(_) => MenuQuestion::new(language.text("More account actions", "更多账号操作"), vec![
                        nav("Rename", "修改名称", language.text("Use a custom name, or clear it to show email", "设置名称，或清除名称以显示邮箱").into(), MenuPage::Rename(index)),
                        action("Use for host sign-in", "设为主登录账号", language.text("Remote owner changes; inference selection stays the same", "更换 Remote 身份；推理选择不变"), MenuOperation::PrimaryUse(index)),
                        action("Sign in again", "重新登录", language.text("Verify this account in your browser; existing credentials are kept until success", "在浏览器完成验证；成功前保留现有凭据"), MenuOperation::Relogin(index)),
                        nav("Enable / remove", "启停 / 移除", "".into(), MenuPage::Membership(index)), back(MenuPage::Actions(index)),
                    ]),
                    MenuPage::Membership(_) => {
                        let mut choices = vec![action(if account.disabled { "Enable" } else { "Disable" }, if account.disabled { "启用" } else { "停用" },
                            language.text("Only affects inference eligibility, not host sign-in", "仅影响推理资格，不改变主登录"), if account.disabled { MenuOperation::Enable(index) } else { MenuOperation::Disable(index) })];
                        if matches!(account.detail, AccountDetail::Subscription { .. }) {
                            choices.extend([action("Clear custom name", "清除自定义名称", language.text("Show email when available", "有邮箱时显示邮箱"), MenuOperation::ClearLabel(index)),
                                action("Remove, keep credentials", "移除并保留凭据", if account.id == "legacy-root" { language.text("Remove pool membership; root sign-in is retained", "移除池成员；保留根登录") } else { language.text("Remove membership; retain local login", "移除池成员；保留本地登录") }, MenuOperation::RemoveKeep(index))]);
                            if account.id != "legacy-root" { choices.push(action("Remove and delete credentials", "移除并删除凭据", language.text("Remove membership and local managed login; sign-in is required to add again", "移除池成员和本地受管登录；再次添加需登录"), MenuOperation::RemoveDelete(index))); }
                        } else { choices.push(nav("Rename", "修改名称", "".into(), MenuPage::Rename(index))); choices.push(action("Remove API and key", "移除 API 与密钥", language.text("Delete this API target and its stored key", "删除此 API 目标及其保存的密钥"), MenuOperation::ApiRemove(index))); }
                        choices.push(back(MenuPage::Actions(index)));
                        MenuQuestion::new(language.text("Edit account", "编辑账号"), choices)
                    }
                    MenuPage::Rename(_) => MenuQuestion { text: format!("{}: {account_name}", language.text("New name", "新名称")), choices: vec![], free_text: true },
                    _ => unreachable!("matched account page"),
                }
    }
}
