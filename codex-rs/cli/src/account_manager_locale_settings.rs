//! Chinese copy for setting choices; stored enum values and JSON are unchanged.

pub(super) fn chinese(english: &str) -> Option<&'static str> {
    Some(match english {
        "Settings\n1 Rotation strategy  2 Early switch percent  3 Return to preferred\n4 Standby warmup  5 Warmup interval  6 Wait for quota recovery\n7 Maximum waiting minutes  8 Automatic reset credits  9 Credit waiting threshold\nB Balanced preset  L Longer standby preset  J Advanced JSON  Enter returns." => {
            "设置\n1 账号切换策略  2 提前切换百分比  3 恢复后返回首选账号\n4 备用账号预热  5 预热间隔  6 等待额度恢复\n7 最长等待分钟数  8 自动使用重置券  9 用券前等待阈值\nB 均衡预设  L 持久备用预设  J 高级 JSON  回车返回。"
        }
        "Presets preserve your reset-credit and paid API choices." => {
            "预设保留已有的重置券和付费 API 设置。"
        }
        "Setting" => "设置项",
        "Preview: {}\nWarmup makes a small request to start eligible standby windows." => {
            "设置预览：{}\n预热会发送一个小请求，启动符合条件的备用账号额度窗口。"
        }
        "Type APPLY to save this preset" => "输入 APPLY 保存此预设",
        "Settings JSON (Enter returns)" => "设置 JSON（回车返回）",
        "Invalid settings JSON: {}" => "设置 JSON 无效：{}",
        "Settings must be an object" => "设置必须是 JSON 对象",
        "Unsupported account pool setting" => "不支持此账号池设置",
        "Keep using an account until early switch or exhaustion" => {
            "持续使用同一账号，直到提前切换或额度耗尽"
        }
        "Prefer the account whose quota resets sooner" => "优先选择额度更早重置的账号",
        "Automatic redemption consumes reset credits only when all eligible subscriptions are exhausted." => {
            "仅在所有符合条件的订阅账号额度耗尽后，自动兑换才会消耗重置券。"
        }
        "Never consume reset credits automatically" => "从不自动使用重置券",
        "Allow one credit after the subscription pool is exhausted" => {
            "订阅账号池耗尽后允许使用一张重置券"
        }
        " (current)" => "（当前）",
        "Choice number (Enter returns)" => "选项编号（回车返回）",
        "Choose a listed number" => "请选择列表中的编号",
        "Type ENABLE to authorize automatic reset-credit use" => "输入 ENABLE 授权自动使用重置券",
        "Return to the preferred account after recovery" => "额度恢复后返回首选账号",
        "Start eligible standby windows with a small quota request" => {
            "使用一个小额度请求启动符合条件的备用账号窗口"
        }
        "Keep an exhausted turn waiting for quota recovery" => "当前回合额度耗尽后继续等待恢复",
        "{}\nCurrent: {}\n1 Enabled  2 Disabled  Enter keeps current." => {
            "{}\n当前：{}\n1 启用  2 停用  回车保持不变。"
        }
        "Choice" => "选项",
        "No change made. Choose 1 or 2." => "未作更改。请选择 1 或 2。",
        "Switch at used percent (0 or 100 disables)" => {
            "已用额度达到多少百分比时切换（0 或 100 关闭）"
        }
        "Warmup interval in minutes" => "预热间隔分钟数",
        "Maximum waiting minutes per turn" => "每回合最长等待分钟数",
        "Natural-reset waiting threshold before using a credit" => {
            "使用重置券前等待自然重置的阈值分钟数"
        }
        "Enter a percentage from 0 to 100" => "请输入 0 到 100 之间的百分比",
        "Warmup interval must be at least 5 minutes" => "预热间隔至少为 5 分钟",
        "Maximum waiting time is 1440 minutes" => "最长等待时间为 1440 分钟",
        "Choose a listed setting or preset." => "请选择菜单中的设置项或预设。",
        _ => return None,
    })
}
