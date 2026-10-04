//! Bounded account metadata and short native-question presentation.

use crate::account_management::AccountManagerInventory;
use crate::account_management::ManagedRateLimitWindow;
use crate::native_account_capabilities::NativeAccountLanguage;
use crate::native_account_capabilities::bounded_text;

#[path = "native_account_actions.rs"]
pub(crate) mod actions;
#[path = "native_account_details.rs"]
mod details;
#[path = "native_account_pages.rs"]
mod pages;

pub(crate) const PAGE_SIZE: usize = 4;
const MAX_ACCOUNTS: usize = 128;

pub(crate) struct FrozenAccountInventory {
    accounts: Vec<FrozenAccount>,
    total: usize,
    paused: bool,
    settings: serde_json::Value,
    primary: Option<crate::account_management::PrimaryLoginView>,
    fallback: codex_login::ApiAccountFallback,
}

#[derive(Clone)]
struct FrozenAccount {
    // Keep exact IDs for operations; only presentation strings are shortened.
    id: String,
    label: String,
    current: bool,
    state: String,
    login_state: String,
    disabled: bool,
    detail: AccountDetail,
}

#[derive(Clone)]
enum AccountDetail {
    Subscription {
        plan: String,
        email: Option<String>,
        primary: Option<ManagedRateLimitWindow>,
        secondary: Option<ManagedRateLimitWindow>,
        primary_observed_at: Option<i64>,
        secondary_observed_at: Option<i64>,
        credits: Option<u64>,
    },
    Api {
        account: codex_login::ApiAccount,
        has_key: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuPage {
    Home,
    Overview(usize),
    Detail(usize),
    Usage(usize),
    Actions(usize),
    More(usize),
    Membership(usize),
    ChoosePage { first: usize, end: usize },
    Browse(usize),
    Settings(usize),
    Primary,
    Refresh(usize),
    Credits(usize, usize),
    Rename(usize),
    Confirm,
    Result,
    Login,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuOperation {
    Reload,
    Automatic,
    Refresh(usize),
    RefreshAccount(usize),
    Use(usize),
    Retry(usize),
    Disable(usize),
    Enable(usize),
    ClearLabel(usize),
    RemoveKeep(usize),
    RemoveDelete(usize),
    Relogin(usize),
    Add,
    PrimaryUse(usize),
    PrimaryRoot,
    PrimaryLogout,
    Credits(usize),
    Redeem(usize, usize),
    ApiUse(usize),
    ApiRemove(usize),
    ApiFallback(usize),
    FallbackOff,
    Setting(usize),
    LoginCheck,
    LoginCancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Page(MenuPage),
    Prepare(MenuOperation),
    Execute(MenuOperation),
    Apply,
    Language,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuChoice {
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) action: MenuAction,
}

pub(crate) struct MenuQuestion {
    // The official mobile client renders this as a heading, so it must stay short.
    pub(crate) text: String,
    pub(crate) choices: Vec<MenuChoice>,
    pub(crate) free_text: bool,
}

impl MenuQuestion {
    fn new(text: impl Into<String>, choices: Vec<MenuChoice>) -> Self {
        Self {
            text: text.into(),
            choices,
            free_text: false,
        }
    }
}

fn choice(label: &str, description: impl Into<String>, action: MenuAction) -> MenuChoice {
    MenuChoice {
        label: bounded_text(label, /*max_chars*/ 56),
        description: bounded_description(&description.into()),
        action,
    }
}

fn bounded_description(value: &str) -> String {
    value
        .lines()
        .take(6)
        .map(|line| bounded_text(line, /*max_chars*/ 160))
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(640)
        .collect()
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
                id: account.profile_id,
                label: bounded_text(&account.label, /*max_chars*/ 80),
                state: account.availability,
                login_state: account.login_state,
                disabled: account.disabled,
                detail: AccountDetail::Subscription {
                    plan: account.plan.unwrap_or_default(),
                    email: account.email,
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
                    id: view.account.id.clone(),
                    label: bounded_text(&view.account.label, /*max_chars*/ 80),
                    state: if view.account.disabled {
                        "disabled"
                    } else {
                        "manual"
                    }
                    .into(),
                    login_state: "api".into(),
                    disabled: view.account.disabled,
                    detail: AccountDetail::Api {
                        account: view.account,
                        has_key: view.has_key,
                    },
                }),
        );
        Self {
            accounts,
            total,
            paused: inventory.paused,
            settings: inventory.settings,
            primary: inventory.primary_login,
            fallback: inventory.api_fallback,
        }
    }

    fn pages(&self) -> usize {
        self.accounts.len().div_ceil(PAGE_SIZE).max(1)
    }

    fn account_choice(
        &self,
        index: usize,
        language: NativeAccountLanguage,
        now: i64,
    ) -> MenuChoice {
        let account = &self.accounts[index];
        let label = format!(
            "{}. {}",
            index + 1,
            bounded_text(&account.label, /*max_chars*/ 38)
        );
        choice(
            &label,
            self.account_summary(account, language, now),
            MenuAction::Page(MenuPage::Detail(index)),
        )
    }

    fn account_summary(
        &self,
        account: &FrozenAccount,
        language: NativeAccountLanguage,
        now: i64,
    ) -> String {
        let state = format!(
            "{}{}",
            if account.current {
                language.text("Current · ", "当前 · ")
            } else {
                ""
            },
            state_name(&account.state, language)
        );
        match &account.detail {
            AccountDetail::Subscription {
                primary,
                secondary,
                primary_observed_at,
                secondary_observed_at,
                ..
            } => format!(
                "{state}\n{} · {}\n{} · {}",
                window_quota(
                    language.text("Primary", "主额度"),
                    primary.as_ref(),
                    now,
                    language
                ),
                observation_age(*primary_observed_at, now, language),
                window_quota(
                    language.text("Secondary", "次额度"),
                    secondary.as_ref(),
                    now,
                    language
                ),
                observation_age(*secondary_observed_at, now, language)
            ),
            AccountDetail::Api { account, .. } => format!(
                "{state}\nAPI · {}\n{}",
                bounded_text(&account.model, /*max_chars*/ 80),
                language.text("Provider charges may apply", "使用可能产生提供商费用")
            ),
        }
    }
}

fn confirmation_target(account: &FrozenAccount, index: usize) -> String {
    format!(
        "{}. {}\nID: {}",
        index + 1,
        bounded_text(&account.label, /*max_chars*/ 56),
        bounded_text(&account.id, /*max_chars*/ 80)
    )
}

fn configured_reset_wait_minutes(settings: &codex_config::AccountPoolConfigToml) -> u64 {
    codex_config::AccountPoolConfigToml {
        resume_after_reset: Some(true),
        ..settings.clone()
    }
    .effective_reset_wait()
    .as_secs()
        / 60
}

fn state_name(state: &str, language: NativeAccountLanguage) -> &'static str {
    match state {
        "ready" => language.text("Ready", "可用"),
        "disabled" => language.text("Disabled", "已停用"),
        "needsLogin" => language.text("Login required", "需要登录"),
        "coolingDown" => language.text("Cooling down", "等待重置"),
        "paused" => language.text("Paused", "已暂停"),
        "manual" => language.text("Manual API", "手动 API"),
        _ => language.text("Unknown", "未知"),
    }
}

fn window_quota(
    name: &str,
    window: Option<&ManagedRateLimitWindow>,
    now: i64,
    language: NativeAccountLanguage,
) -> String {
    format!(
        "{}: {}",
        window_name(name, window, language),
        quota(window, now, language)
    )
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
        language.text("just now", "刚刚记录").into()
    } else if age < 3600 {
        format!("{} {}", age / 60, language.text("m old", "分钟前"))
    } else if age < 86_400 {
        format!("{} {}", age / 3600, language.text("h old", "小时前"))
    } else {
        format!("{} {}", age / 86_400, language.text("d old", "天前"))
    }
}

#[cfg(test)]
#[path = "native_account_view_tests.rs"]
mod tests;
