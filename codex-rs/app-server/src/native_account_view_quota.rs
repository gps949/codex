//! A request receipt never replaces either quota window's observation time.

use super::*;

pub(super) fn status_description(
    account: &FrozenAccount,
    language: NativeAccountLanguage,
    now: i64,
) -> Option<String> {
    let AccountDetail::Subscription {
        refresh,
        cooldown_until,
        backend_resets_at,
        ..
    } = &account.detail
    else {
        return None;
    };
    let mut lines = Vec::new();
    if let Some(refresh) = refresh {
        lines.push(format!(
            "{}: {}",
            language.text("Last quota check", "最近额度查询"),
            observation_age(Some(refresh.attempted_at), now, language)
        ));
        lines.push(if refresh.in_progress {
            language
                .text("Checking backend quota…", "正在查询后端额度…")
                .into()
        } else {
            bounded_text(
                &refresh_message(&refresh.message, language),
                /*max_chars*/ 160,
            )
        });
    }
    if account.state == "coolingDown" {
        lines.push(match cooldown_until {
            Some(until) if *until > now => format!(
                "{}: {}",
                language.text("Local cooldown until", "本地冷却至"),
                timestamp(*until)
            ),
            Some(_) => language
                .text(
                    "Cooldown timer elapsed; refresh quota to check current permission",
                    "冷却计时已结束；刷新额度以确认当前使用权限",
                )
                .into(),
            None => language
                .text("Local cooldown is active", "本地冷却仍生效")
                .into(),
        });
        lines.push(
            language
                .text(
                    "Cached percentages do not confirm recovery after a quota refusal",
                    "缓存百分比不能确认此前额度拒绝已经恢复",
                )
                .into(),
        );
        if let Some(reset) = backend_resets_at {
            lines.push(format!(
                "{}: {}",
                language.text("Backend reset reported", "已报告后端重置时间"),
                timestamp(*reset)
            ));
        }
        lines.push(
            language
                .text(
                    "Refresh both windows; after an external reset, use the retry action",
                    "刷新两个窗口；若已在外部重置，可使用重试操作",
                )
                .into(),
        );
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn refresh_message(message: &str, language: NativeAccountLanguage) -> String {
    if language == NativeAccountLanguage::English {
        return message.into();
    }
    let translated = match message {
        "Quota updated" => "额度已更新",
        "Backend recovery confirmed" => "后端已确认额度恢复",
        "Backend denies ordinary usage; cached percentages do not grant permission. Wait for reset or inspect available reset credits." => {
            "后端仍拒绝普通使用；缓存百分比不代表可用。等待重置或查看可用重置券。"
        }
        "Backend reports a quota or workspace restriction. Inspect account access or reset credits before retrying." => {
            "后端报告额度或工作区限制，请检查访问权限或重置券。"
        }
        "Both quota windows updated; local recovery remains unconfirmed. Refresh again, or retry after an external reset." => {
            "两个额度窗口均已更新，但恢复仍未确认。再次刷新，或在外部重置后使用重试。"
        }
        "Quota check timed out; cached values were retained. Refresh to retry." => {
            "额度查询超时，保留原缓存；请刷新重试。"
        }
        _ => {
            if language == NativeAccountLanguage::Chinese
                && let Some(windows) = message.strip_suffix(". Refresh again; cached values kept.")
            {
                let mut translated = Vec::new();
                for window in windows.split("; ") {
                    let (name, status) =
                        if let Some(status) = window.strip_prefix("Primary window ") {
                            ("主额度", status)
                        } else if let Some(status) = window.strip_prefix("Secondary window ") {
                            ("次额度", status)
                        } else {
                            return message.into();
                        };
                    let status = match status {
                        "updated" => "已更新",
                        "not refreshed (omitted by backend)" => "未更新（后端未返回）",
                        "not refreshed (older or conflicting response)" => {
                            "未更新（响应较旧或有冲突）"
                        }
                        "remains stale or incomplete" => "仍过期或不完整",
                        _ => return message.into(),
                    };
                    translated.push(format!("{name}{status}"));
                }
                return format!("{}。保留原缓存，请再次刷新。", translated.join("；"));
            }
            return message.into();
        }
    };
    translated.into()
}
