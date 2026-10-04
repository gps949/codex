//! Explicit interface language for the standalone account manager only.

#[path = "account_manager_locale_actions.rs"]
mod actions;
#[path = "account_manager_locale_settings.rs"]
mod settings;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Locale {
    #[default]
    #[value(name = "en")]
    English,
    #[value(name = "zh-CN", alias = "zh", alias = "zh-cn")]
    SimplifiedChinese,
}

impl Locale {
    pub(crate) fn from_preference(
        language: codex_app_server::account_management::ManagerLanguage,
    ) -> Self {
        match language {
            codex_app_server::account_management::ManagerLanguage::English => Self::English,
            codex_app_server::account_management::ManagerLanguage::SimplifiedChinese => {
                Self::SimplifiedChinese
            }
        }
    }

    pub(super) fn preference(self) -> codex_app_server::account_management::ManagerLanguage {
        match self {
            Self::English => codex_app_server::account_management::ManagerLanguage::English,
            Self::SimplifiedChinese => {
                codex_app_server::account_management::ManagerLanguage::SimplifiedChinese
            }
        }
    }
    pub(super) fn toggle(self) -> Self {
        match self {
            Self::English => Self::SimplifiedChinese,
            Self::SimplifiedChinese => Self::English,
        }
    }

    /// Translates code-owned copy, never an account label or other user data.
    pub(super) fn text(self, english: &'static str) -> &'static str {
        match self {
            Self::English => english,
            Self::SimplifiedChinese => chinese(english).unwrap_or(english),
        }
    }

    /// Interpolates values once, preserving their contents even if they contain braces.
    pub(super) fn format(self, english: &'static str, values: &[&str]) -> String {
        let template = self.text(english);
        debug_assert_eq!(template.matches("{}").count(), values.len());
        let mut parts = template.split("{}");
        let mut output = parts.next().unwrap_or_default().to_string();
        for value in values {
            output.push_str(value);
            output.push_str(parts.next().unwrap_or_default());
        }
        output
    }

    /// Only exact, known backend messages are translated; diagnostic details stay intact.
    pub(super) fn message(self, message: &str) -> &str {
        match self {
            Self::English => message,
            Self::SimplifiedChinese => chinese(message).unwrap_or(message),
        }
    }

    pub(super) fn notice(self, message: &str) -> String {
        if self == Self::SimplifiedChinese
            && let Some(summary) = message
                .strip_prefix("Quota check: ")
                .and_then(|text| text.strip_suffix(". Each account shows its own result."))
            && let Some((updated, rest)) = summary.split_once(" updated, ")
            && let Some((failed, rest)) = rest.split_once(" failed, ")
            && let Some(checking) = rest.strip_suffix(" already checking")
            && let (Ok(updated), Ok(failed), Ok(checking)) = (
                updated.parse::<u64>(),
                failed.parse::<u64>(),
                checking.parse::<u64>(),
            )
        {
            return self.format(
                "Quota check: {} updated, {} failed, {} already checking. Each account shows its own result.",
                &[&updated.to_string(), &failed.to_string(), &checking.to_string()]
            );
        }
        self.message(message).to_string()
    }

    pub(super) fn main_menu(self) -> &'static str {
        self.text("[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[H] Host sign-in  [C] Cancel login  [number] Account actions  [G] Language / 中文  [Q] Quit")
    }
}

fn chinese(english: &str) -> Option<&'static str> {
    let translated = match english {
        "Language saved for browser and terminal managers." => {
            "已保存语言，浏览器和终端管理界面共用此设置。"
        }
        "Language changed for this session; saving failed: {}" => {
            "本次界面已切换语言，但保存失败：{}"
        }
        "Host sign-in / Remote Control" => "主登录 / 远程控制",
        "Root login" => "根目录登录",
        "Signed out" => "已退出登录",
        "Host sign-in unavailable" => "主登录不可用",
        "Selected account unavailable" => "所选账号不可用",
        "Host sign-in selected. Inference selection is unchanged. Remote Control may need reconnection or pairing for the new owner." => {
            "已选择主登录，推理账号选择未更改。远程控制可能需要为新身份重新连接或配对。"
        }
        "Host sign-in now uses root login. Inference selection is unchanged." => {
            "主登录已改为根目录登录，推理账号选择未更改。"
        }
        "Host signed out. Pool credentials were retained." => "已退出主登录，账号池凭据仍保留。",
        "This profile is selected for host sign-in. Choose another host login or run codex account primary logout before removing it." => {
            "此档案正在用作主登录。移除前请另选主登录，或运行 codex account primary logout。"
        }
        "Host sign-in and inference selection are independent." => "主登录与推理账号选择相互独立。",
        "H manages host sign-in. It is independent from inference selection." => {
            "按 H 管理主登录；主登录与推理账号选择相互独立。"
        }
        "Codex Accounts" => "Codex 账号管理",
        "Automatic subscriptions" => "自动选择订阅账号",
        "API target unavailable" => "API 目标账号不可用",
        "Manual API: {} (provider billed)" => "手动 API：{}（由提供商计费）",
        "Target: {}" => "当前目标：{}",
        "Subscription pool: {}" => "订阅账号池：{}",
        "Enabled" => "已启用",
        "Disabled" => "已停用",
        "Paused" => "已暂停",
        "{} subscription accounts · {} ready · {} checking" => {
            "{} 个订阅账号 · {} 个可用 · {} 个正在检查"
        }
        "Quota percentages show USED allowance; ? means not checked." => {
            "百分比表示已用额度；? 表示尚未检查。"
        }
        "Next: {}" => "下一步：{}",
        "O returns to automatic subscriptions. API requests are provider billed." => {
            "按 O 返回自动选择订阅账号。API 请求由提供商计费。"
        }
        "O resumes subscription selection." => "按 O 恢复订阅账号选择。",
        "A adds your first subscription account." => "按 A 添加第一个订阅账号。",
        "Choose an account number, then D to enable it." => "输入账号编号，再按 D 启用账号。",
        "Choose an account number, then L to finish login." => "输入账号编号，再按 L 完成登录。",
        "R checks for restored quota without spending a reset credit." => {
            "按 R 检查额度是否恢复，不消耗重置券。"
        }
        "Choose an account number to view quota, credits and actions." => {
            "输入账号编号，查看额度、重置券和可用操作。"
        }
        "ACCOUNT" => "账号",
        "PLAN" => "订阅",
        "AVAILABILITY" => "状态",
        "PRIMARY" => "短期",
        "WEEKLY" => "长期",
        "CREDITS" => "重置券",
        "Unknown" => "未知",
        "Unknown plan" => "订阅未知",
        "Primary {} · Weekly {}" => "短期 {} · 长期 {}",
        "Reset credits {}" => "重置券 {}",
        "Checking fresh quota…" => "正在检查最新额度…",
        "Check failed: {}" => "检查失败：{}",
        "No subscription accounts. Choose Add to begin." => "暂无订阅账号。按 A 添加账号。",
        "API accounts · fallback {}" => "API 账号 · 最终兜底{}",
        "explicitly enabled" => "已明确启用",
        "off" => "已关闭",
        "disabled" => "已停用",
        "key missing" | "Key missing" => "缺少密钥",
        "provider billed" | "Provider billed" => "提供商计费",
        "Ready" => "可用",
        "Waiting reset" => "等待额度重置",
        "Needs login" => "需要登录",
        "Check status" => "请检查状态",
        "Choose" => "请选择",
        "Account label" => "账号名称",
        "Login number (Enter returns)" => "登录任务编号（回车返回）",
        "Choose a listed login number" => "请选择列表中的登录任务编号",
        "Choose an account number or a listed action." => "请输入账号编号或菜单中的操作字母。",
        "[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[H] Host sign-in  [C] Cancel login  [number] Account actions  [G] Language / 中文  [Q] Quit" => {
            "[R] 刷新额度  [A] 添加账号  [S] 设置  [P] API 账号  [O] 自动选择订阅\n[H] 主登录  [C] 取消登录  [编号] 账号操作  [G] 语言 / Language  [Q] 退出"
        }
        "Login {} · {}" => "登录任务 {} · {}",
        "Open: {}" => "打开：{}",
        "Verification code: {}" => "验证码：{}",
        "Press Enter to update the account list." => "按回车更新账号列表。",
        "Terminal input closed" => "终端输入已关闭",
        "API key (hidden)" => "API 密钥（输入隐藏）",
        "Key entry cancelled" => "已取消密钥输入",
        "API key is too long" => "API 密钥过长",
        "Invalid API key paste" => "粘贴的 API 密钥无效",
        "Requesting browser verification code" => "正在获取浏览器验证码",
        "Waiting for browser verification" => "等待浏览器验证",
        "Login cancelled" => "登录已取消",
        "Signed in: {}" => "登录成功：{}",
        "Account selected for subsequent requests." => "后续请求将使用所选账号。",
        "Local quota cooldown cleared for one probe. No reset credit was used." => {
            "已清除本地额度冷却，允许检查一次。未使用重置券。"
        }
        "Returned to subscription account selection. Exhausted accounts retain their cooldowns." => {
            "已返回订阅账号选择。耗尽账号仍保留冷却状态。"
        }
        "Account details saved. Running clients synchronize the change." => {
            "账号信息已保存，运行中的客户端会同步更改。"
        }
        "Account removed from this pool. Server revocation was attempted when local credentials were removed." => {
            "账号已从池中移除。移除本地凭据时会尝试撤销服务端授权。"
        }
        "Open the verification link and complete account login." => {
            "请打开验证链接并完成账号登录。"
        }
        "Login cancellation requested." => "已请求取消登录。",
        "Reset credits loaded for the selected account." => "已读取所选账号的重置券。",
        "Pool settings saved. Active sessions apply them after configuration refresh." => {
            "账号池设置已保存，活动会话将在配置刷新后应用。"
        }
        "Backend recovery confirmed" => "已确认服务端额度恢复",
        "Quota updated" => "额度已更新",
        "Quota check: {} updated, {} failed, {} already checking. Each account shows its own result." => {
            "额度检查：{} 个已更新，{} 个失败，{} 个正在检查。每个账号显示各自的结果。"
        }
        "Quota check timed out; cached values were retained" => "额度检查超时，已保留缓存信息",
        "Quota check interrupted; refresh to try again" => "额度检查已中断，请刷新重试",
        "Enable this account before selecting it" => "请先启用账号再选择",
        "Complete account login first" | "Account login is incomplete" | "Account needs login" => {
            "请先完成账号登录"
        }
        "Account profile no longer exists" => "该账号记录已不存在",
        "Login operation no longer exists" => "该登录任务已不存在",
        "ChatGPT login is disabled by the authentication policy" => "认证策略已禁用 ChatGPT 登录",
        "Account manager is shutting down" => "账号管理器正在关闭",
        "Finish or cancel an existing login first" => "请先完成或取消一个已有登录任务",
        "This account already has a login in progress" => "该账号已有登录任务正在进行",
        "Account is disabled; enable it before contacting the backend" => {
            "账号已停用，请先启用再联系服务端"
        }
        "Reset credits are available only for ChatGPT subscription accounts" => {
            "重置券仅适用于 ChatGPT 订阅账号"
        }
        "Reset credit redeemed and quota recovery confirmed for this account." => {
            "重置券已使用，并已确认该账号额度恢复。"
        }
        "Reset credit redeemed. Checking fresh quota before confirming local recovery." => {
            "重置券已使用，正在检查最新额度以确认恢复。"
        }
        "Backend reported a partial or unconfirmed reset. Checking fresh quota." => {
            "服务端报告部分重置或结果待确认，正在检查最新额度。"
        }
        "Backend reported no new redemption. Checking quota before confirming recovery." => {
            "服务端未报告新的兑换，正在检查额度以确认恢复。"
        }
        "No reset was applied. Refresh quota or inspect available credits." => {
            "未执行重置，请刷新额度或查看可用重置券。"
        }
        "Reset outcome is unconfirmed. Refresh quota before retrying the same operation ID." => {
            "重置结果待确认。请先刷新额度，再使用同一操作编号重试。"
        }
        _ => return actions::chinese(english).or_else(|| settings::chinese(english)),
    };
    Some(translated)
}

#[cfg(test)]
#[path = "account_manager_locale_tests.rs"]
mod tests;
