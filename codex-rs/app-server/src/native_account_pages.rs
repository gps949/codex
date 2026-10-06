//! Phone-sized account pages: headings are brief, body data uses option descriptions.

use super::*;

impl FrozenAccountInventory {
    pub(crate) fn question(&self, page: MenuPage, language: NativeAccountLanguage) -> MenuQuestion {
        self.question_at(page, language, chrono::Utc::now().timestamp())
    }

    pub(super) fn question_at(
        &self,
        page: MenuPage,
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
        match page {
            MenuPage::Quick(_) | MenuPage::QuickOptions(_) | MenuPage::Strategy(_) => {
                self.quick_question(page, language, now)
            }
            MenuPage::Home => {
                let mut choices = vec![
                    nav(
                        "Accounts",
                        "账号列表",
                        format!(
                            "{} {} · {}{}",
                            self.total,
                            language.text("accounts", "个账号"),
                            if self.paused {
                                language.text("Pool paused", "账号池已暂停")
                            } else {
                                language.text(
                                    "Cached quota; refresh to check changes",
                                    "缓存额度；可刷新确认变化",
                                )
                            },
                            if self.total > self.accounts.len() {
                                language.text(" · Showing first 128", " · 仅显示前 128 个")
                            } else {
                                ""
                            }
                        ),
                        MenuPage::Overview(0),
                    ),
                    nav(
                        "Pool settings",
                        "账号池设置",
                        language
                            .text(
                                "Selection, warmup, reset and login",
                                "选择、预热、重置与添加账号",
                            )
                            .into(),
                        MenuPage::Settings(0),
                    ),
                    nav(
                        "Model selection",
                        "模型选择",
                        self.routing
                            .as_ref()
                            .map(|view| routing::summary(view, language))
                            .unwrap_or_else(|| {
                                language
                                    .text(
                                        "Policy unavailable; reload to retry",
                                        "策略不可用；可重载重试",
                                    )
                                    .into()
                            }),
                        MenuPage::Routing(RoutingPage::Mode),
                    ),
                    nav(
                        "Host sign-in",
                        "主登录账号",
                        self.primary_summary(language, now),
                        MenuPage::Primary,
                    ),
                    choice(
                        language.text("中文", "English"),
                        language.text("Remember this interface language", "记住界面语言"),
                        MenuAction::Language,
                    ),
                    close(),
                ];
                if !self.reset_journals.is_empty() {
                    choices.insert(
                        1,
                        nav(
                            "Interrupted resets",
                            "中断的重置",
                            language
                                .text(
                                    "Review original records; outcomes remain unknown",
                                    "复核原记录；结果仍保持未知",
                                )
                                .into(),
                            MenuPage::ResetReviews(0),
                        ),
                    );
                }
                MenuQuestion::new(language.text("Manage accounts", "管理账号"), choices)
            }
            MenuPage::Routing(page) => self.routing_question(page, language),
            page @ (MenuPage::ResetReviews(_) | MenuPage::ResetRecord(_)) => {
                self.reset_review_question(page, language)
            }
            MenuPage::Overview(page) => {
                let page = page.min(self.pages() - 1);
                let mut choices = (page * PAGE_SIZE
                    ..self.accounts.len().min((page + 1) * PAGE_SIZE))
                    .map(|index| self.account_choice(index, language, now))
                    .collect::<Vec<_>>();
                if self.accounts.is_empty() {
                    return MenuQuestion::new(
                        language.text("No accounts yet", "尚未添加账号"),
                        vec![
                            action(
                                "Add subscription account",
                                "添加订阅账号",
                                language.text("Start browser verification", "开始浏览器验证"),
                                MenuOperation::Add,
                            ),
                            read(
                                "Reload inventory",
                                "重载账号列表",
                                language.text(
                                    "Read saved accounts and settings; keep cached quota",
                                    "读取保存的账号与设置；保留缓存额度",
                                ),
                                MenuOperation::Reload,
                            ),
                            back(MenuPage::Home),
                            close(),
                        ],
                    );
                }
                choices.push(nav(
                    "More / pages",
                    "更多 / 翻页",
                    language
                        .text(
                            "Navigate, refresh this page or return",
                            "翻页、刷新本页或返回",
                        )
                        .into(),
                    MenuPage::Browse(page),
                ));
                MenuQuestion::new(
                    if self.accounts.is_empty() {
                        language.text("No accounts yet", "尚未添加账号").into()
                    } else {
                        format!(
                            "{} · {}/{}",
                            language.text("Choose an account", "选择账号"),
                            page + 1,
                            self.pages()
                        )
                    },
                    choices,
                )
            }
            MenuPage::Browse(page) => MenuQuestion::new(
                language.text("Account list options", "列表操作"),
                vec![
                    nav(
                        "Next page",
                        "下一页",
                        "".into(),
                        MenuPage::Overview((page + 1) % self.pages()),
                    ),
                    nav(
                        "Choose page",
                        "选择页码",
                        "".into(),
                        MenuPage::ChoosePage {
                            first: 0,
                            end: self.pages(),
                            origin: MenuOrigin::Overview(page),
                        },
                    ),
                    read(
                        "Refresh this page",
                        "刷新本页",
                        language.text(
                            "Check signed-in subscription accounts; no reset credit is used",
                            "查询已登录订阅账号；不会使用重置券",
                        ),
                        MenuOperation::Refresh(page * PAGE_SIZE),
                    ),
                    read(
                        "Reload inventory",
                        "重载账号列表",
                        language.text(
                            "Read saved accounts and settings; keep cached quota",
                            "读取保存的账号与设置；保留缓存额度",
                        ),
                        MenuOperation::Reload,
                    ),
                    back(MenuPage::Home),
                ],
            ),
            MenuPage::ChoosePage { first, end, origin } => {
                let (pages, stride) = match origin {
                    MenuOrigin::Quick(_) => (self.quick_pages(), QUICK_PAGE_SIZE),
                    MenuOrigin::Overview(_) | MenuOrigin::Settings(_) => (self.pages(), PAGE_SIZE),
                };
                let list_page = |page| match origin {
                    MenuOrigin::Quick(_) => MenuPage::Quick(page),
                    MenuOrigin::Overview(_) | MenuOrigin::Settings(_) => MenuPage::Overview(page),
                };
                let first = first.min(pages - 1);
                let end = end.min(pages).max(first + 1);
                let mut choices = Vec::new();
                if end - first <= 3 {
                    for page in first..end {
                        choices.push(choice(
                            &format!("{} {}", language.text("Page", "第"), page + 1),
                            format!(
                                "{}–{}",
                                page * stride + 1,
                                ((page + 1) * stride).min(match origin {
                                    MenuOrigin::Quick(_) => self.quick_indices().len(),
                                    MenuOrigin::Overview(_) | MenuOrigin::Settings(_) =>
                                        self.accounts.len(),
                                })
                            ),
                            MenuAction::Page(list_page(page)),
                        ));
                    }
                } else {
                    let middle = first + (end - first).div_ceil(2);
                    for (first, end) in [(first, middle), (middle, end)] {
                        choices.push(choice(
                            &format!("{} {}–{}", language.text("Pages", "页码"), first + 1, end),
                            "",
                            MenuAction::Page(MenuPage::ChoosePage { first, end, origin }),
                        ));
                    }
                }
                choices.push(back(origin.page()));
                MenuQuestion::new(language.text("Choose a page", "选择页码"), choices)
            }
            MenuPage::Detail(index)
            | MenuPage::Usage(index)
            | MenuPage::Actions(index)
            | MenuPage::More(index)
            | MenuPage::Membership(index)
            | MenuPage::Rename(index) => self.account_page(page, index, language, now),
            MenuPage::Primary => {
                let status = self.primary_summary(language, now);
                MenuQuestion::new(
                    language.text("Host sign-in", "主登录账号"),
                    vec![
                        nav(
                            "Choose pool account",
                            "选择池中账号",
                            format!(
                                "{status}\n{}",
                                language.text(
                                    "Choose an account, then More actions → Use for host sign-in",
                                    "选择账号后：更多操作 → 设为主登录账号"
                                )
                            ),
                            MenuPage::Overview(0),
                        ),
                        action(
                            "Use root login",
                            "使用根登录",
                            language.text(
                                "Switch host source to its root login; no inference change",
                                "主登录改用根登录；推理选择不变",
                            ),
                            MenuOperation::PrimaryRoot,
                        ),
                        action(
                            "Sign out host",
                            "退出主登录",
                            language.text(
                                "Disconnect Remote; retain pool credentials",
                                "断开 Remote；保留池账号凭据",
                            ),
                            MenuOperation::PrimaryLogout,
                        ),
                        back(MenuPage::Home),
                        close(),
                    ],
                )
            }
            MenuPage::Settings(page) => {
                let Ok(settings) = serde_json::from_value::<codex_config::AccountPoolConfigToml>(
                    self.settings.clone(),
                ) else {
                    return MenuQuestion::new(
                        language.text("Settings unavailable", "设置不可用"),
                        vec![
                            read(
                                "Reload inventory",
                                "重载账号列表",
                                language.text(
                                    "Read saved accounts and settings again",
                                    "重新读取保存的账号与设置",
                                ),
                                MenuOperation::Reload,
                            ),
                            back(MenuPage::Home),
                        ],
                    );
                };
                let toggle = |en, zh, enabled, setting| {
                    action(
                        en,
                        zh,
                        if enabled {
                            language.text(
                                "Currently on; confirm to turn off",
                                "当前已开启；确认后关闭",
                            )
                        } else {
                            language.text(
                                "Currently off; confirm to turn on",
                                "当前已关闭；确认后开启",
                            )
                        },
                        MenuOperation::Setting(setting),
                    )
                };
                let wait = configured_reset_wait_minutes(&settings);
                let active_wait = settings.effective_reset_wait().as_secs() / 60;
                let wait_description = format!(
                    "{}: {wait} {} · {}: {active_wait} {}\n{}",
                    language.text("Limit", "上限"),
                    language.text("min", "分钟"),
                    language.text("Active wait", "实际等待"),
                    language.text("min", "分钟"),
                    if wait == 0 {
                        language.text(
                            "Confirm to restore the default 360-minute limit",
                            "确认后恢复默认 360 分钟上限",
                        )
                    } else {
                        language.text(
                            "Confirm to disable natural-reset waiting (0 min)",
                            "确认后关闭等待自然重置（0 分钟）",
                        )
                    }
                );
                let choices = match page {
                    0 => vec![nav("Rotation strategy", "轮换策略", match settings.effective_rotation_strategy() {
                            codex_config::AccountPoolRotationStrategy::FillFirst => language.text("By priority", "按优先级"),
                            codex_config::AccountPoolRotationStrategy::EarliestReset => language.text("By reset time", "按重置时间"),
                        }.into(), MenuPage::Strategy(MenuOrigin::Settings(0))),
                        toggle("Window warmup", "窗口预热", settings.effective_window_warmup(), 1), toggle("Resume after reset", "重置后接续", settings.resume_after_reset.unwrap_or(true), 2),
                        nav("More settings", "更多设置", "".into(), MenuPage::Settings(1)), back(MenuPage::Home)],
                    1 => vec![action("Reset wait budget", "等待重置时长", &wait_description, MenuOperation::Setting(3)),
                        action("Automatic reset credits", "自动使用重置券", match settings.effective_auto_reset_credits() {
                            codex_config::AutoResetCredits::Never => language.text("Currently never; confirm to allow redemption after pool exhaustion", "当前从不用券；确认后允许订阅池耗尽后兑换"),
                            codex_config::AutoResetCredits::WhenPoolExhausted => language.text("Currently after pool exhaustion; confirm to never redeem automatically", "当前订阅池耗尽后用券；确认后不再自动兑换"),
                        }, MenuOperation::Setting(4)),
                        toggle("Return to preferred", "回到首选账号", settings.effective_return_to_preferred(), 5), nav("Pool actions", "账号池操作", "".into(), MenuPage::Settings(2)), back(MenuPage::Settings(0))],
                    _ => vec![choice(language.text("Use subscriptions automatically", "自动使用订阅池"), language.text("Exit manual API selection; preserve quota cooldowns", "退出手动 API 选择；保留额度冷却"), MenuAction::Automatic),
                        action("Add subscription account", "添加订阅账号", language.text("Start browser verification on this host", "开始主机上的浏览器验证流程"), MenuOperation::Add),
                        nav("Refresh pool in batches", "分批刷新账号池", language.text("Check at most four accounts per confirmation", "每次最多查询 4 个账号").into(), MenuPage::Refresh(0)),
                        action("Disable paid fallback", "关闭付费兜底", language.text("Keep third-party API available for manual selection only", "第三方 API 仅保留手动选择"), MenuOperation::FallbackOff), back(MenuPage::Settings(1))],
                };
                MenuQuestion::new(language.text("Pool settings", "账号池设置"), choices)
            }
            MenuPage::Refresh(first) => {
                let first = first.min(self.accounts.len().saturating_sub(1));
                let mut choices = vec![read(
                    "Check this batch",
                    "查询本批",
                    language.text(
                        "At most four subscription accounts; API targets are skipped",
                        "最多查询 4 个订阅账号；跳过 API",
                    ),
                    MenuOperation::Refresh(first),
                )];
                if self.accounts.len() > PAGE_SIZE {
                    let next = if first + PAGE_SIZE < self.accounts.len() {
                        first + PAGE_SIZE
                    } else {
                        0
                    };
                    choices.push(nav(
                        "Next batch",
                        "下一批",
                        format!(
                            "{}–{}",
                            next + 1,
                            (next + PAGE_SIZE).min(self.accounts.len())
                        ),
                        MenuPage::Refresh(next),
                    ));
                }
                choices.push(back(MenuPage::Settings(2)));
                MenuQuestion::new(
                    language.text("Refresh quota batch", "分批刷新额度"),
                    choices,
                )
            }
            MenuPage::Credits(_, _) | MenuPage::Confirm | MenuPage::Result | MenuPage::Login => {
                unreachable!("session-owned page")
            }
        }
    }
    fn primary_summary(&self, language: NativeAccountLanguage, now: i64) -> String {
        let Some(view) = &self.primary else {
            return language
                .text(
                    "Host sign-in is independent of inference",
                    "主登录独立于推理账号池",
                )
                .into();
        };
        let label = match (view.source.as_str(), view.status.as_str()) {
            ("root", "runtimeResolutionRequired") => {
                language.text("Host-managed identity", "主机受管身份")
            }
            ("root", _) => language.text("Root login", "根登录"),
            ("signedOut", _) => language.text("Signed out", "已退出登录"),
            ("invalid", _) => language.text("Unavailable", "不可用"),
            _ => &view.label,
        };
        let status = match view.status.as_str() {
            "storedReady" => language.text("Credentials available", "凭据可用"),
            "runtimeResolutionRequired" => {
                language.text("Resolved by running host", "由运行中的主机确认")
            }
            "signedOut" => language.text("Signed out", "已退出登录"),
            _ => language.text("Needs attention", "需要处理"),
        };
        let mut summary = format!(
            "{}: {label}\n{status}",
            language.text("Saved source", "保存的来源")
        );
        if let Some(runtime) = view.runtime.as_ref().filter(|runtime| {
            runtime.source_revision == view.revision
                && runtime.observed_at <= now
                && now.saturating_sub(runtime.observed_at) < 10
        }) {
            summary.push_str(&format!(
                "\n{}: {}\nRemote: {}",
                language.text("Observed host", "运行中身份"),
                runtime
                    .email
                    .as_deref()
                    .unwrap_or(language.text("Not reported", "未报告")),
                match runtime.remote_status.as_str() {
                    "disabled" => language.text("Disabled", "已关闭"),
                    "connecting" => language.text("Connecting", "连接中"),
                    "connected" => language.text("Connected to relay", "已连接中继"),
                    "errored" => language.text("Needs attention", "需要处理"),
                    "requirementsDisabled" => language.text(
                        "Remote disabled by account requirements",
                        "账号策略禁止 Remote"
                    ),
                    "authenticationDenied" => language.text(
                        "Host authentication denied by requirements",
                        "主登录认证被账号策略拒绝"
                    ),
                    _ => language.text("Not reported", "未报告"),
                }
            ));
        } else {
            summary.push_str(language.text(
                "\nNo recent running-host confirmation",
                "\n尚无运行中主机的近期确认",
            ));
        }
        summary
    }
}
