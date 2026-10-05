//! Reversible subscription choices fit a phone-sized native question.

use super::*;
use codex_config::AccountPoolRotationStrategy;

impl FrozenAccountInventory {
    pub(super) fn quick_indices(&self) -> Vec<usize> {
        let mut indices = self
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, account)| matches!(account.detail, AccountDetail::Subscription { .. }))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by_key(|index| !self.accounts[*index].current);
        indices
    }

    pub(super) fn quick_pages(&self) -> usize {
        self.quick_indices().len().div_ceil(QUICK_PAGE_SIZE).max(1)
    }

    pub(super) fn rotation_strategy(&self) -> anyhow::Result<AccountPoolRotationStrategy> {
        let settings: codex_config::AccountPoolConfigToml =
            serde_json::from_value(self.settings.clone())?;
        Ok(settings.effective_rotation_strategy())
    }

    pub(super) fn quick_question(
        &self,
        page: MenuPage,
        language: NativeAccountLanguage,
        now: i64,
    ) -> MenuQuestion {
        let nav =
            |label, description: String, page| choice(label, description, MenuAction::Page(page));
        let strategy_name = |strategy| match strategy {
            AccountPoolRotationStrategy::FillFirst => language.text("By priority", "按优先级"),
            AccountPoolRotationStrategy::EarliestReset => {
                language.text("By reset time", "按重置时间")
            }
        };
        match page {
            MenuPage::Quick(page) => {
                let page = page.min(self.quick_pages() - 1);
                let indices = self.quick_indices();
                let mut choices = indices
                    .iter()
                    .skip(page * QUICK_PAGE_SIZE)
                    .take(QUICK_PAGE_SIZE)
                    .enumerate()
                    .map(|(offset, index)| {
                        let account = &self.accounts[*index];
                        let AccountDetail::Subscription { plan, email, .. } = &account.detail
                        else {
                            unreachable!("subscription quick list")
                        };
                        let label = format!(
                            "{}. {}",
                            page * QUICK_PAGE_SIZE + offset + 1,
                            bounded_text(&account.label, /*max_chars*/ 38)
                        );
                        let description = format!(
                            "{}\n{} · {}",
                            self.account_summary(account, language, now),
                            bounded_text(plan, /*max_chars*/ 40),
                            bounded_text(
                                email
                                    .as_deref()
                                    .unwrap_or(language.text("Email unknown", "邮箱未知")),
                                /*max_chars*/ 96
                            )
                        );
                        let action = if !account.disabled
                            && account.login_state == "signedIn"
                            && matches!(account.state.as_str(), "ready" | "paused")
                        {
                            MenuAction::SelectSubscription(*index)
                        } else {
                            MenuAction::Page(MenuPage::Detail(*index))
                        };
                        choice(&label, description, action)
                    })
                    .collect::<Vec<_>>();
                let strategy = self
                    .rotation_strategy()
                    .map(strategy_name)
                    .unwrap_or(language.text("Strategy unavailable", "策略不可用"));
                let selection = if self.accounts.iter().any(|account| {
                    account.current && matches!(account.detail, AccountDetail::Api { .. })
                }) {
                    language.text(
                        "Paid API selected; return to subscriptions",
                        "当前为付费 API；返回订阅",
                    )
                } else {
                    language.text("Subscriptions", "订阅账号池")
                };
                choices.extend([
                    choice(
                        language.text("Choose automatically", "自动选择"),
                        format!(
                            "{strategy} · {selection}\n{}",
                            language.text(
                                "Preserve quota cooldowns; use eligible subscription accounts",
                                "保留额度冷却；使用符合条件的订阅账号"
                            )
                        ),
                        MenuAction::Automatic,
                    ),
                    nav(
                        language.text("Strategy", "轮换策略"),
                        strategy.into(),
                        MenuPage::Strategy(MenuOrigin::Quick(page)),
                    ),
                    nav(
                        language.text("More", "更多"),
                        language
                            .text(
                                "Pages, quota refresh and full management",
                                "翻页、刷新额度与完整管理",
                            )
                            .into(),
                        MenuPage::QuickOptions(page),
                    ),
                ]);
                MenuQuestion::new(
                    format!(
                        "{} · {}/{}",
                        language.text("Accounts", "账号"),
                        page + 1,
                        self.quick_pages()
                    ),
                    choices,
                )
            }
            MenuPage::QuickOptions(page) => {
                let page = page.min(self.quick_pages() - 1);
                let mut choices = Vec::new();
                if self.quick_pages() > 1 {
                    choices.extend([
                        nav(
                            language.text("Next page", "下一页"),
                            String::new(),
                            MenuPage::Quick((page + 1) % self.quick_pages()),
                        ),
                        nav(
                            language.text("Previous page", "上一页"),
                            String::new(),
                            MenuPage::Quick((page + self.quick_pages() - 1) % self.quick_pages()),
                        ),
                    ]);
                }
                choices.extend([
                    choice(
                        language.text("Refresh this page", "刷新本页"),
                        language.text(
                            "Check only these subscriptions; no reset credit is used",
                            "仅查询本页订阅账号；不会使用重置券",
                        ),
                        MenuAction::Execute(MenuOperation::RefreshQuick(page)),
                    ),
                    nav(
                        language.text("Manage accounts", "管理账号"),
                        language
                            .text(
                                "Login, account details and explicit account actions",
                                "登录、账号详情与明确账号操作",
                            )
                            .into(),
                        MenuPage::Home,
                    ),
                    nav(
                        language.text("Back", "返回"),
                        String::new(),
                        MenuPage::Quick(page),
                    ),
                ]);
                if self.quick_pages() == 1 {
                    choices.push(choice(
                        language.text("Close", "关闭"),
                        "",
                        MenuAction::Close,
                    ));
                }
                MenuQuestion::new(language.text("Account options", "账号选项"), choices)
            }
            MenuPage::Strategy(origin) => {
                let Ok(current) = self.rotation_strategy() else {
                    return MenuQuestion::new(
                        language.text("Strategy unavailable", "策略不可用"),
                        vec![
                            choice(
                                language.text("Reload inventory", "重载账号列表"),
                                "",
                                MenuAction::Execute(MenuOperation::Reload),
                            ),
                            nav(language.text("Back", "返回"), String::new(), origin.page()),
                        ],
                    );
                };
                let choices = [
                    AccountPoolRotationStrategy::FillFirst,
                    AccountPoolRotationStrategy::EarliestReset,
                ]
                .into_iter()
                .map(|strategy| {
                    choice(
                        strategy_name(strategy),
                        format!(
                            "{}{}",
                            if current == strategy {
                                language.text("Current · ", "当前 · ")
                            } else {
                                ""
                            },
                            match strategy {
                                AccountPoolRotationStrategy::FillFirst => language.text(
                                    "Use the lowest priority number first",
                                    "优先使用优先级数字较小的账号"
                                ),
                                AccountPoolRotationStrategy::EarliestReset => language.text(
                                    "Start idle windows, then prefer the soonest reset",
                                    "先启动未开始的窗口，再优先使用最早重置的账号"
                                ),
                            }
                        ),
                        MenuAction::SetStrategy(strategy),
                    )
                })
                .chain([
                    nav(language.text("Back", "返回"), String::new(), origin.page()),
                    choice(language.text("Close", "关闭"), "", MenuAction::Close),
                ])
                .collect();
                MenuQuestion::new(language.text("Rotation strategy", "轮换策略"), choices)
            }
            _ => unreachable!("quick account page"),
        }
    }
}
