//! Chinese copy for guided account actions; machine tokens remain unchanged.

pub(super) fn chinese(english: &str) -> Option<&'static str> {
    Some(match english {
        "Saved source: {}" => "保存的登录来源：{}",
        "Resolved by running host" => "由运行中的宿主解析",
        "External host sign-in is resolved by the running host. Stored root credentials are not its current identity." => {
            "外部主登录由运行中的宿主解析；保存的根登录凭据不代表其当前身份。"
        }
        "Host-managed workload identity is resolved by the running host." => {
            "工作负载身份由运行中的宿主解析。"
        }
        "Stored credentials: {}" => "保存的凭据：{}",
        "Available" => "可用",
        "Needs attention" => "需要处理",
        "The phone must use the matching account and workspace. A new owner may require pairing again." => {
            "手机需使用对应账号和工作区；主登录身份改变后可能需要重新配对。"
        }
        "{} · {} · {}" => "{} · {} · {}",
        "Signed in" => "已登录",
        "Disabled inference accounts can still be used for host sign-in." => {
            "推理中停用的账号仍可用作主登录。"
        }
        "[number] Choose host account  [R] Root login  [X] Sign out host  [Enter] Back" => {
            "[编号] 选择主登录账号  [R] 根目录登录  [X] 退出主登录  [回车] 返回"
        }
        "Host sign-in action" => "主登录操作",
        "Use root login for host sign-in? Type APPLY" => "将根目录登录用作主登录？输入 APPLY 确认",
        "Sign out host and disconnect Remote Control? Type SIGNOUT" => {
            "退出主登录并断开远程控制？输入 SIGNOUT 确认"
        }
        "Choose a listed account number" => "请选择列表中的账号编号",
        "Account: {}" => "账号：{}",
        "Observed host identity: {}" => "主机实际观测身份：{}",
        "Remote service: {}" => "远程服务：{}",
        "Not reported" => "未报告",
        "Connecting" => "正在连接",
        "Connected to relay" => "已连接远程中继",
        "Remote disabled by account requirements" => "账号策略禁止 Remote",
        "Host authentication denied by requirements" => "主登录认证被账号策略拒绝",
        "Connection needs attention" => "连接需要处理",
        "No recent host confirmation for this selection. Stored credentials do not confirm Remote Control is connected." => {
            "主机尚未近期确认此选择；已保存凭据不代表远程控制已连接。"
        }
        "Standby warmup" => "备用账号预热",
        "Warmup uses a small generating request. Viewing this page and refreshing quota do not start warmup." => {
            "预热会产生少量生成请求。查看本页与刷新额度不会启动预热。"
        }
        "Latest evidence: {}" => "最新证据：{}",
        "Last attempt: {}" => "上次尝试：{}",
        "Next eligible check: {}" => "下次最早可检查时间：{}",
        "Consecutive failures: {}" => "连续失败次数：{}",
        "No recent warmup attempt recorded. A quota window may also start during normal use." => {
            "暂无近期预热记录；正常使用也可能启动额度窗口。"
        }
        "A completed request or reset timestamp alone does not confirm a quota window started; positive current usage does." => {
            "仅请求完成或重置时间不能确认窗口已启动；当前正用量才能提供证据。"
        }
        "Current quota confirms the window is active" => "当前额度证实窗口正在运行",
        "Warmup request is running" => "预热请求正在运行",
        "Request sent; window start unconfirmed" => "请求已发出，窗口启动待确认",
        "Request completed; window start unconfirmed" => "请求已完成，窗口启动待确认",
        "Login is needed before warmup" => "预热前需完成登录",
        "Deferred until the next eligible check" => "已延后，等待下次可检查时间",
        "Request failed; waiting before retry" => "请求失败，等待重试",
        "A retry is eligible when the scheduler runs" => "调度器运行时可再次尝试",
        "Earlier evidence has expired" => "此前记录已过期",
        "No confirmed warmup evidence" => "暂无已确认的预热证据",
        "Use this account for host sign-in? Type APPLY" => {
            "将此账号用作主登录？输入 APPLY 确认，推理选择不变；已启用的远程服务会重连，手机可能需重新配对。"
        }
        "Use this account for host sign-in?" => {
            "将此账号用作主登录？推理选择不变，远程连接可能需要重连或配对。"
        }
        "Profile: {}" => "档案：{}",
        "Email: {}" => "邮箱：{}",
        "E edits a custom name; Enter keeps it. N clears it and uses email, then profile ID." => {
            "E 编辑自定义名称；回车保留原名称。N 清空自定义名称，自动使用邮箱，邮箱不可用时使用档案 ID。"
        }
        "Custom label (Enter keeps the current name)" => "自定义名称（回车保留原名称）",
        "Status: {} · Priority: {}" => "状态：{} · 优先级：{}",
        "Quota percentages show used allowance. Refresh checks the backend without starting a task or spending a credit." => {
            "百分比表示已用额度。刷新只检查服务端，不启动任务，也不消耗重置券。"
        }
        "Short window" => "短期额度窗口",
        "Long window" => "长期额度窗口",
        "{}% used" => "已用 {}%",
        "Unknown usage" => "用量未知",
        "{}: {} · Duration {} minutes" => "{}：{} · 窗口长度 {} 分钟",
        "unknown" => "未知",
        "Window start unconfirmed. A reset timestamp alone does not prove warmup completed." => {
            "窗口是否开始尚未确认。仅有重置时间不能证明预热已完成。"
        }
        "Reported reset in {}h {}m" => "服务端报告将在 {} 小时 {} 分钟后重置",
        "Reset time reached; refresh to confirm current allowance." => {
            "已到重置时间，请刷新确认当前额度。"
        }
        "Cached timestamp is ahead of this host's clock; refresh quota." => {
            "缓存时间晚于主机当前时间，请刷新额度。"
        }
        "Cached observation: {} minutes ago" => "缓存观测：{} 分钟前",
        "{}: Not checked. R refreshes quota." => "{}：尚未检查。按 R 刷新额度。",
        "Last check: {}" => "上次检查：{}",
        "[U] Use  [H] Host sign-in  [R] Refresh  [T] Retry after external reset  [C] Reset credits  [W] Warmup details  [L] Relogin  [E] Edit  [N] Automatic name  [D] Enable/disable  [X] Remove  [Enter] Back" => {
            "[U] 推理使用  [H] 主登录  [R] 刷新  [T] 外部重置后重试  [C] 重置券  [W] 预热详情  [L] 重新登录  [E] 编辑  [N] 自动名称  [D] 启用/停用  [X] 移除  [回车] 返回"
        }
        "Action" => "操作",
        "Label" => "账号名称",
        "Priority" => "优先级",
        "Remove this account? Type REMOVE" => "移除此账号？输入 REMOVE 确认",
        "Keep local credentials? y/n" => "保留本地凭据？y/n",
        "API accounts\n[A] Add  [U] Select  [E] Edit  [K] Replace key  [D] Enable/disable  [X] Remove\n[O] Automatic subscriptions  [F] Configure final fallback  [Enter] Back" => {
            "API 账号\n[A] 添加  [U] 选择  [E] 编辑  [K] 更换密钥  [D] 启用/停用  [X] 移除\n[O] 自动选择订阅  [F] 设置最终兜底  [回车] 返回"
        }
        "The endpoint must support Responses. The key is read from stdin without printing it." => {
            "接口必须支持 Responses。密钥输入不会显示在终端中。"
        }
        "HTTPS endpoint" => "HTTPS 接口地址",
        "Model" => "模型",
        "API account number" => "API 账号编号",
        "Invalid account number" => "账号编号无效",
        "Remove this account and its local key? Type REMOVE" => {
            "移除此账号及其本地密钥？输入 REMOVE 确认"
        }
        "Context window" => "上下文窗口长度",
        "Image input supported? y/n" => "支持图片输入？y/n",
        "Final API fallback incurs provider charges and sends this conversation to that provider." => {
            "最终 API 兜底由提供商计费，并会将当前对话发送给该提供商。"
        }
        "Type ENABLE to turn on, DISABLE to turn off, or Enter to return" => {
            "输入 ENABLE 启用、DISABLE 关闭，或按回车返回"
        }
        "No change made. Type ENABLE or DISABLE." => "未作更改。请输入 ENABLE 或 DISABLE。",
        "Choose an enabled API account with a saved key" => "请选择已启用且已保存密钥的 API 账号",
        "Subscription waiting minutes before fallback" => "进入兜底前等待订阅恢复的分钟数",
        "Enter to return" => "按回车返回",
        "A previous reset has an unconfirmed outcome. Credit: {} · Operation: {}" => {
            "上次重置结果待确认。重置券：{} · 操作编号：{}"
        }
        "R refreshes quota. RETRY uses the same credit and operation ID. REVIEW clears the pending record only after you check its outcome." => {
            "R 刷新额度。RETRY 使用同一张券和操作编号重试。REVIEW 仅在核实结果后清除待确认记录。"
        }
        "Choose R, RETRY, REVIEW, or Enter to return" => "输入 R、RETRY、REVIEW，或按回车返回",
        "Original credit status: {}" => "原重置券状态：{}",
        "No longer listed" => "已不在列表中",
        "Clear only after checking the provider's quota and credit history. This forgets the pending record; it does not undo a redemption." => {
            "请先检查提供商的额度和重置券历史，再清除记录。此操作仅清除待确认记录，不会撤销兑换。"
        }
        "Type REVIEWED to confirm you checked the outcome" => "输入 REVIEWED 确认已核实结果",
        "Credit list unavailable" => "无法获取重置券列表",
        "No available reset credits. Refresh quota or wait for the natural reset." => {
            "暂无可用重置券。请刷新额度，或等待自然重置。"
        }
        "{}: {} · {} · Expires {}" => "{}：{} · {} · 到期时间 {}",
        "Reset credit" => "重置券",
        "Unknown scope" => "适用范围未知",
        "No expiry reported" => "未报告到期时间",
        "Codex quota" => "Codex 额度",
        "Credit number (Enter returns)" => "重置券编号（回车返回）",
        "Choose a listed credit number" => "请选择列表中的重置券编号",
        "This consumes one reset credit for {}." => "此操作会为 {} 使用一张重置券。",
        "Type REDEEM to confirm" => "输入 REDEEM 确认使用",
        "Credit ID missing" => "缺少重置券编号",
        "Reset operation: {}" => "重置操作编号：{}",
        "The operation ID was retained. Refresh quota before retrying." => {
            "已保留操作编号。重试前请先刷新额度。"
        }
        "Enter a whole number" => "请输入整数",
        "Enter a number" => "请输入数字",
        "API account saved for manual selection. No generating request was sent." => {
            "API 账号已保存，可手动选择。未发送生成请求。"
        }
        "API account details updated." => "API 账号信息已更新。",
        "API key replaced for subsequent turns. No generating request was sent." => {
            "后续回合将使用新 API 密钥。未发送生成请求。"
        }
        "Manual API target selected for subsequent turns. Usage is billed by this provider." => {
            "后续回合将使用所选 API 账号，用量由该提供商计费。"
        }
        "API account and its local key removed." => "API 账号及其本地密钥已移除。",
        "API fallback policy saved. Automatic subscription selection remains the default." => {
            "API 兜底策略已保存，默认仍自动选择订阅账号。"
        }
        "The managed workspace policy does not allow selecting a third-party API target" => {
            "工作区管理策略不允许选择第三方 API 账号"
        }
        "The managed workspace policy does not allow a third-party fallback" => {
            "工作区管理策略不允许第三方 API 兜底"
        }
        "available" => "可用",
        "consumed" | "used" => "已使用",
        "expired" => "已过期",
        "unavailable" => "不可用",
        _ => return None,
    })
}
