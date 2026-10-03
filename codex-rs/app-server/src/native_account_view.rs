//! Frozen, metadata-only pages for native account controls.

use crate::account_management::AccountManagerInventory;
use crate::account_management::ManagedRateLimitWindow;
use crate::native_account_capabilities::NativeAccountLanguage;
use crate::native_account_capabilities::bounded_text;

const PAGE_SIZE: usize = 5;
const MAX_ACCOUNTS: usize = 128;

pub(crate) struct FrozenAccountInventory {
    accounts: Vec<FrozenAccount>,
    total: usize,
    paused: bool,
    observed_at: i64,
}

struct FrozenAccount {
    id: String,
    label: String,
    current: bool,
    state: String,
    detail: AccountDetail,
}

enum AccountDetail {
    Subscription {
        plan: String,
        email: String,
        primary: Option<ManagedRateLimitWindow>,
        secondary: Option<ManagedRateLimitWindow>,
        primary_observed_at: Option<i64>,
        secondary_observed_at: Option<i64>,
        credits: Option<u64>,
    },
    Api {
        model: String,
        has_key: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuPage {
    Home,
    Overview(usize),
    Detail(usize),
    ChoosePage { first: usize, end: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Home,
    Overview(usize),
    Detail(usize),
    ChoosePage { first: usize, end: usize },
    Language,
    Close,
}

pub(crate) struct MenuQuestion {
    pub(crate) text: String,
    pub(crate) choices: Vec<(String, MenuAction)>,
}

impl FrozenAccountInventory {
    pub(crate) fn from_inventory(inventory: AccountManagerInventory) -> Self {
        let api_active = match &inventory.api_selection {
            codex_login::ApiAccountSelection::Subscription => None,
            codex_login::ApiAccountSelection::Manual { profile_id } => Some(profile_id),
        };
        let total = inventory
            .accounts
            .len()
            .saturating_add(inventory.api_accounts.len());
        let mut accounts = inventory
            .accounts
            .into_iter()
            .take(MAX_ACCOUNTS)
            .map(|account| FrozenAccount {
                current: api_active.is_none()
                    && inventory.active_profile_id.as_ref() == Some(&account.profile_id),
                id: bounded_text(&account.profile_id, /*max_chars*/ 96),
                label: bounded_text(&account.label, /*max_chars*/ 64),
                state: account.availability,
                detail: AccountDetail::Subscription {
                    plan: account
                        .plan
                        .map(|value| bounded_text(&value, /*max_chars*/ 64))
                        .unwrap_or_default(),
                    email: account
                        .email
                        .map(|value| bounded_text(&value, /*max_chars*/ 96))
                        .unwrap_or_default(),
                    primary: account.rate_limits.primary,
                    secondary: account.rate_limits.secondary,
                    primary_observed_at: account.rate_limits.primary_observed_at,
                    secondary_observed_at: account.rate_limits.secondary_observed_at,
                    credits: account.reset_credit_count,
                },
            })
            .collect::<Vec<_>>();
        let remaining = MAX_ACCOUNTS.saturating_sub(accounts.len());
        accounts.extend(
            inventory
                .api_accounts
                .into_iter()
                .take(remaining)
                .map(|view| FrozenAccount {
                    current: api_active == Some(&view.account.id),
                    id: bounded_text(&view.account.id, /*max_chars*/ 96),
                    label: bounded_text(&view.account.label, /*max_chars*/ 64),
                    state: if view.account.disabled {
                        "disabled"
                    } else {
                        "manual"
                    }
                    .into(),
                    detail: AccountDetail::Api {
                        model: bounded_text(&view.account.model, /*max_chars*/ 96),
                        has_key: view.has_key,
                    },
                }),
        );
        Self {
            accounts,
            total,
            paused: inventory.paused,
            observed_at: inventory.host_now,
        }
    }

    pub(crate) fn question(&self, page: MenuPage, language: NativeAccountLanguage) -> MenuQuestion {
        let choose = |en, zh, action| (language.text(en, zh).into(), action);
        match page {
            MenuPage::Home => MenuQuestion {
                text: format!("{}\n{}: {}\n{}", language.text("Account manager · read-only", "账号管理 · 只读"),
                    language.text("Accounts", "账号数"), self.total,
                    language.text("View cached quota and account details without using model quota or credits.", "查看缓存额度与账号详情；不会消耗模型额度或重置券。")),
                choices: vec![choose("Show overview", "查看概览", MenuAction::Overview(0)),
                    choose("中文", "English", MenuAction::Language), choose("Close", "关闭", MenuAction::Close)],
            },
            MenuPage::Overview(page) => {
                let pages = self.accounts.len().div_ceil(PAGE_SIZE).max(1);
                let page = page.min(pages - 1);
                let start = page * PAGE_SIZE;
                let mut text = format!("{} · {}/{}\n{}\n", language.text("Account overview", "账号概览"), page + 1, pages,
                    language.text("Cached snapshot; quota may have changed.", "缓存快照；实际额度可能已变化。"));
                if self.paused { text.push_str(language.text("Pool paused\n", "账号池已暂停\n")); }
                for (index, account) in self.accounts.iter().enumerate().skip(start).take(PAGE_SIZE) {
                    text.push_str(&format!("\n{}. {}{} · {}\n", index + 1, account.label,
                        if account.current { language.text(" · Current", " · 当前") } else { "" }, state_name(&account.state, language)));
                    match &account.detail {
                        AccountDetail::Subscription { primary, secondary, primary_observed_at, secondary_observed_at, .. } => {
                            for (name, window, observed_at) in [
                                (language.text("Primary", "主额度"), primary, *primary_observed_at),
                                (language.text("Secondary", "次额度"), secondary, *secondary_observed_at),
                            ] {
                                text.push_str(&format!("   {}: {} · {}\n", window_name(name, window.as_ref(), language),
                                    quota(window.as_ref(), self.observed_at, language), observation_age(observed_at, self.observed_at, language)));
                            }
                        }
                        AccountDetail::Api { model, .. } => text.push_str(&format!("   API · {model}\n")),
                    }
                }
                if self.accounts.is_empty() { text.push_str(language.text("No enrolled accounts.\n", "尚未添加账号。\n")); }
                if self.total > self.accounts.len() { text.push_str(language.text("Only the first 128 accounts are shown.\n", "仅显示前 128 个账号。\n")); }
                text.push_str(&format!("\n{}: {}", language.text("Local snapshot created", "本地快照创建时间"), timestamp(self.observed_at)));
                let mut choices = Vec::new();
                if !self.accounts.is_empty() { choices.push(choose("Account details", "账号详情", MenuAction::Detail(start))); }
                if pages > 2 { choices.push(choose("Choose page", "选择页码", MenuAction::ChoosePage { first: 0, end: pages })); }
                else if pages > 1 { choices.push(choose("Next page", "下一页", MenuAction::Overview((page + 1) % pages))); }
                if self.accounts.is_empty() { choices.push(choose("Back", "返回", MenuAction::Home)); }
                choices.push(choose("Close", "关闭", MenuAction::Close));
                MenuQuestion { text, choices }
            }
            MenuPage::Detail(index) => {
                let Some(account) = self.accounts.get(index) else { return self.question(MenuPage::Home, language); };
                let mut text = format!("{} · {}/{}\n{}\nID: {}\n{}: {}", language.text("Account details", "账号详情"), index + 1, self.accounts.len(),
                    account.label, account.id, language.text("State", "状态"), state_name(&account.state, language));
                match &account.detail {
                    AccountDetail::Subscription { plan, email, primary, secondary, primary_observed_at, secondary_observed_at, credits } => {
                        text.push_str(&format!("\n{}: {plan}\n{}: {email}", language.text("Subscription", "订阅"), language.text("Email", "邮箱")));
                        for (name, window, observed_at) in [
                            (language.text("Primary", "主额度"), primary, *primary_observed_at),
                            (language.text("Secondary", "次额度"), secondary, *secondary_observed_at),
                        ] {
                            text.push_str(&format!("\n{}: {}", window_name(name, window.as_ref(), language), quota(window.as_ref(), self.observed_at, language)));
                            if let Some(at) = window.as_ref().and_then(|window| window.resets_at) { text.push_str(&format!(" · {} {}", language.text("Reset", "重置"), timestamp(at))); }
                            text.push_str(&format!("\n  {}: {} · {}", language.text("Observed", "记录时间"),
                                observed_at.map(timestamp).unwrap_or_else(|| language.text("Unknown", "未知").into()),
                                observation_age(observed_at, self.observed_at, language)));
                        }
                        if let Some(credits) = credits { text.push_str(&format!("\n{}: {credits}", language.text("Reset credits (cached)", "重置券（缓存）"))); }
                    }
                    AccountDetail::Api { model, has_key } => text.push_str(&format!("\nAPI · {model}\n{}\n{}",
                        if *has_key { language.text("Key configured", "已配置密钥") } else { language.text("Key missing", "缺少密钥") },
                        language.text("Subscription quota does not apply. API use may incur charges.", "不适用订阅额度。API 使用可能产生费用。"))),
                }
                text.push_str(language.text("\n\nRead-only snapshot. Reopen the menu to load current local data.", "\n\n只读快照。重新打开菜单可加载最新本地数据。"));
                let mut choices = vec![choose("Overview", "概览", MenuAction::Overview(index / PAGE_SIZE))];
                if self.accounts.len() > 1 { choices.push(choose("Next account", "下一个账号", MenuAction::Detail((index + 1) % self.accounts.len()))); }
                choices.push(choose("Close", "关闭", MenuAction::Close));
                MenuQuestion { text, choices }
            }
            MenuPage::ChoosePage { first, end } => {
                let pages = self.accounts.len().div_ceil(PAGE_SIZE).max(1);
                let first = first.min(pages - 1);
                let end = end.min(pages).max(first + 1);
                let text = format!("{} · {}–{}\n{}", language.text("Choose a page", "选择页码"), first + 1, end,
                    language.text("Select a page range, then a page. All captured accounts are reachable.", "先选择页码范围，再选择一页。可查看所有已捕获的账号。"));
                let mut choices = Vec::new();
                if end - first <= 2 {
                    for page in first..end {
                        choices.push((format!("{} {} · {}–{}", language.text("Page", "第"), page + 1,
                            page * PAGE_SIZE + 1, ((page + 1) * PAGE_SIZE).min(self.accounts.len())), MenuAction::Overview(page)));
                    }
                } else {
                    let middle = first + (end - first).div_ceil(2);
                    for (range_first, range_end) in [(first, middle), (middle, end)] {
                        choices.push((format!("{} {}–{}", language.text("Pages", "页码"), range_first + 1, range_end),
                            MenuAction::ChoosePage { first: range_first, end: range_end }));
                    }
                }
                choices.push(choose("Back", "返回", MenuAction::Overview(first)));
                MenuQuestion { text, choices }
            }
        }
    }
}

fn state_name(state: &str, language: NativeAccountLanguage) -> &'static str {
    match state {
        "ready" => language.text("Ready", "可用"),
        "disabled" => language.text("Disabled", "已禁用"),
        "needsLogin" => language.text("Login required", "需要登录"),
        "coolingDown" => language.text("Cooling down", "等待重置"),
        "paused" => language.text("Paused", "已暂停"),
        "manual" => language.text("Manual API", "手动 API"),
        _ => language.text("Unknown", "未知"),
    }
}

fn quota(
    window: Option<&ManagedRateLimitWindow>,
    now: i64,
    language: NativeAccountLanguage,
) -> String {
    match window.filter(|window| window.used_percent.is_finite()) {
        Some(window) => {
            let used = window.used_percent.clamp(0.0, 100.0);
            let percentage = if used > 0.0 && used < 1.0 {
                "<1%".into()
            } else if used > 99.0 && used < 100.0 {
                ">99%".into()
            } else {
                format!("{used:.0}%")
            };
            if window.resets_at.is_some_and(|reset| reset <= now) {
                format!(
                    "{} ({percentage} {})",
                    language.text("Stale", "已过期"),
                    language.text("cached", "缓存")
                )
            } else {
                format!("{percentage} {}", language.text("used", "已用"))
            }
        }
        None => language.text("Unknown", "未知").into(),
    }
}

fn timestamp(value: i64) -> String {
    chrono::DateTime::from_timestamp(value, /*nsecs*/ 0)
        .map(|time| time.format("%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "Unknown".into())
}

fn window_name(
    name: &str,
    window: Option<&ManagedRateLimitWindow>,
    language: NativeAccountLanguage,
) -> String {
    let Some(minutes) = window
        .and_then(|window| window.window_minutes)
        .filter(|minutes| *minutes > 0)
    else {
        return name.into();
    };
    let (value, unit) = if minutes % 1440 == 0 {
        (minutes / 1440, language.text("d", " 天"))
    } else if minutes % 60 == 0 {
        (minutes / 60, language.text("h", " 小时"))
    } else {
        (minutes, language.text("m", " 分钟"))
    };
    format!("{name} ({value}{unit})")
}

fn observation_age(observed_at: Option<i64>, now: i64, language: NativeAccountLanguage) -> String {
    let Some(at) = observed_at else {
        return language.text("age unknown", "记录时间未知").into();
    };
    if at > now {
        return language
            .text("timestamp ahead of host clock", "记录时间晚于主机时钟")
            .into();
    }
    let age = now.saturating_sub(at);
    if age < 60 {
        language.text("recorded just now", "刚刚记录").into()
    } else if age < 3600 {
        format!("{} {}", age / 60, language.text("m old", "分钟前记录"))
    } else if age < 86_400 {
        format!("{} {}", age / 3600, language.text("h old", "小时前记录"))
    } else {
        format!("{} {}", age / 86_400, language.text("d old", "天前记录"))
    }
}

#[cfg(test)]
#[path = "native_account_view_tests.rs"]
mod tests;
