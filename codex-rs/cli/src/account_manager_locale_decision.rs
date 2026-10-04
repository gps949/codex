//! Default-English copy for independent decision-service setup.

pub(super) fn chinese(english: &str) -> Option<&'static str> {
    Some(match english {
        "[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[D] Jev / Clef  [H] Host sign-in  [C] Cancel login  [number] Account actions\n[G] Language / 中文  [Q] Quit" => {
            "[R] 刷新  [A] 添加  [S] 设置  [P] API 账号  [O] 自动订阅账号\n[D] Jev / Clef  [H] 主登录  [C] 取消登录  [序号] 账号操作\n[G] 语言 / English  [Q] 退出"
        }
        "Decision assistance" => "决策辅助",
        "Service: {}" => "服务商：{}",
        "Mode: {}" => "模式：{}",
        "Observe only" => "仅观察",
        "Token available" => "令牌可用",
        "Token not configured" => "尚未配置令牌",
        "Settings apply to subsequent advisor calls. Other hosts have not been observed." => {
            "设置适用于后续辅助请求；其他宿主的实际状态尚未观测。"
        }
        "[S] Set up Jev / Clef  [T] Test saved connection  [X] Disable  [Enter] Back" => {
            "[S] 设置 Jev / Clef  [T] 测试已保存连接  [X] 关闭  [Enter] 返回"
        }
        "[S] Choose one service  [T] Test saved connection  [X] Disable  [K] Remove saved token  [Enter] Back" => {
            "[S] 选择一个服务  [T] 测试已保存连接  [X] 关闭  [K] 删除已保存令牌  [Enter] 返回"
        }
        "Type REMOVE to delete this service token and disable assistance" => {
            "输入 REMOVE 删除此服务令牌并关闭辅助"
        }
        "Choose one service; only that service is called." => "选一个服务即可，只会调用该服务。",
        "Decision assistance action" => "决策辅助操作",
        "Testing sends only built-in example text and may incur a separate service fee." => {
            "测试仅发送内置示例文本，可能产生独立服务费用。"
        }
        "Type TEST to send one connection check" => "输入 TEST 发送一次连接检查",
        "1 Cloudflare Clef  2 TypeSafe Jev  Enter returns" => {
            "1 Cloudflare Clef  2 TypeSafe Jev  Enter 返回"
        }
        "Service number" => "服务商序号",
        "Cloudflare Account ID" => "Cloudflare 账号 ID",
        "Enter the Cloudflare Account ID shown on the Workers AI REST API page" => {
            "请输入 Workers AI REST API 页面上的 Cloudflare 账号 ID"
        }
        "1 Clef-flash  2 Clef" => "1 Clef-flash  2 Clef",
        "Model number" => "模型序号",
        "Enter a separate service token. Leave blank to keep the token for this exact service." => {
            "输入独立服务令牌。留空保留为此准确服务目标保存的令牌。"
        }
        "1 Disabled  2 Observe only  3 Enabled" => "1 关闭  2 仅观察  3 启用",
        "Mode number" => "模式序号",
        "Observe only sends requests but keeps the original results. Enabled applies validated suggestions." => {
            "仅观察会发请求但保留原结果；启用后才应用经过校验的建议。"
        }
        "Suggest skills too? 1 Yes / 2 No" => "同时提供技能建议？1 是 / 2 否",
        "Cloud services receive search text and tool or skill descriptions, and may charge separately. Saving sends no request." => {
            "云端服务会收到搜索文本及工具或技能说明，可能独立计费。保存不会发请求。"
        }
        "Type SAVE to confirm these settings" => "输入 SAVE 确认设置",
        "Choose a listed action" => "请选择列出的操作",
        "Decision settings saved. Subsequent advisor calls use the updated configuration; no service request was sent." => {
            "决策辅助设置已保存，后续辅助请求使用更新配置；未发送服务请求。"
        }
        "Connection check succeeded. Only built-in example text was sent; settings were not saved." => {
            "连接检查成功，仅发送了内置示例文本；未保存设置。"
        }
        "Decision settings saved locally. Higher-priority settings override this manager's effective configuration." => {
            "设置已保存；更高优先级设置覆盖了此管理器的配置。"
        }
        "Decision settings saved locally. Updated hosts read them on the next decision call; actual execution adoption has not been observed." => {
            "设置已保存；新版宿主将在下次辅助请求时读取，实际采用状态尚未观测。"
        }
        "Synthetic connection test succeeded. Draft settings were not saved." => {
            "示例连接测试成功；尚未保存设置。"
        }
        "No independent decision credential is available for this service target." => {
            "此服务目标尚无独立令牌，请输入并保存。"
        }
        "Decision service destination is blocked by managed network policy." => {
            "受管网络策略禁止访问此服务。"
        }
        "Synthetic connection test timed out." => "连接测试超时。",
        "Synthetic connection test could not reach the service or authenticate." => {
            "无法连接或认证，请检查账号 ID、令牌和服务地址。"
        }
        "Enter a token for this service before enabling decision assistance." => {
            "请先为此服务输入令牌，再启用决策辅助。"
        }
        _ => return None,
    })
}
