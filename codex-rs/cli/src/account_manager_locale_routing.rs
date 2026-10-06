//! Default-English labels for task-model routing controls.

pub(super) fn chinese(english: &str) -> Option<&'static str> {
    Some(match english {
        "[R] Refresh  [A] Add  [S] Settings  [P] API accounts  [O] Automatic subscriptions\n[M] Model selection  [D] Jev / Clef  [H] Host sign-in  [C] Cancel login  [number] Account actions\n[G] Language / 中文  [Q] Quit" => {
            "[R] 刷新  [A] 添加  [S] 设置  [P] API 账号  [O] 自动订阅账号\n[M] 模型选择  [D] Jev / Clef  [H] 主登录  [C] 取消登录  [序号] 账号操作\n[G] 语言 / English  [Q] 退出"
        }
        "Task model selection" => "任务模型选择",
        "Saved mode: {}" => "已保存模式：{}",
        "Preview only" => "仅预览",
        "Automatic model selection" => "自动选择模型",
        "Source: {}" => "决策来源：{}",
        "Local rules" => "本地规则",
        "Jev / Clef service" => "Jev / Clef 服务",
        "On" => "开启",
        "Off" => "关闭",
        "Main tasks: {} · Subagents: {}" => "主任务：{} · 子代理：{}",
        "Balance: {} / 100 · Highest effort: {}" => "倾向：{} / 100 · 最高思考强度：{}",
        "0 favors endurance; 100 favors capability. Model roles are relative preferences, not measured quota costs." => {
            "0 偏续航；100 偏能力。模型角色表示相对偏好，并非实测配额消耗。"
        }
        "Decision service ready; task-text consent is separate from tool assistance." => {
            "决策服务已配置；发送任务描述需要独立同意，与工具辅助无关。"
        }
        "Decision service missing; use D to configure Jev / Clef first." => {
            "尚未配置决策服务；请先按 D 设置 Jev / Clef。"
        }
        "Task-text consent: {} · Local fallback: {}" => "发送任务描述：{} · 本地规则兜底：{}",
        "Effective mode: {} · Higher-priority settings override the saved policy." => {
            "实际模式：{} · 更高优先级配置覆盖已保存策略。"
        }
        "New tasks read this policy. Manual model choices and inherited full-history children keep their model." => {
            "新任务读取此策略。手动指定模型和继承完整历史的子代理保留其模型。"
        }
        "Preview records a suggestion; Automatic applies a validated choice. Local simulation sends no requests." => {
            "预览仅记录建议；自动模式应用经过校验的选择。本地模拟不发送请求。"
        }
        "[0] Off  [1] Preview  [2] Automatic\n[S] Source  [T] Main tasks  [A] Subagents  [B] Balance  [E] Effort\n[M] Models / roles  [P] Local simulation  [F] Local fallback  [D] Jev / Clef  [Enter] Back" => {
            "[0] 关闭  [1] 预览  [2] 自动\n[S] 来源  [T] 主任务  [A] 子代理  [B] 倾向  [E] 思考强度\n[M] 模型 / 角色  [P] 本地模拟  [F] 本地兜底  [D] Jev / Clef  [Enter] 返回"
        }
        "Model selection action" => "模型选择操作",
        "Balance (0 to 100)" => "倾向（0 到 100）",
        "Enter a whole number from 0 to 100" => "请输入 0 到 100 的整数",
        "1 minimal  2 low  3 medium  4 high  5 xhigh  6 max" => {
            "1 minimal  2 low  3 medium  4 high  5 xhigh  6 max"
        }
        "Effort number (Enter returns)" => "思考强度序号（回车返回）",
        "1 Local rules  2 Jev / Clef service" => "1 本地规则  2 Jev / Clef 服务",
        "Source number (Enter returns)" => "来源序号（回车返回）",
        "Local simulation uses no history and sends nothing to Jev / Clef." => {
            "本地模拟不使用会话历史，也不向 Jev / Clef 发送数据。"
        }
        "Example task (Enter returns)" => "示例任务（回车返回）",
        "This service receives up to 2048 bytes of each eligible task description and may charge separately. No history, source files, account details or quota is sent." => {
            "服务会收到符合条件任务的描述（最多 2048 字节），可能独立计费。不发送历史、源码、账号详情或额度。"
        }
        "Type ROUTE to allow task-text requests" => "输入 ROUTE 同意发送任务描述",
        "No valid local choice; keep the current model." => "没有有效的本地建议；保留当前模型。",
        "Local suggestion: {} · {}" => "本地建议：{} · {}",
        "Last applied choice: {} · {}" => "最近已应用选择：{} · {}",
        "Last recorded suggestion: {} · {}" => "最近记录的建议：{} · {}",
        "No eligible catalog models; automatic selection keeps the current model." => {
            "尚无符合条件的目录模型；自动选择会保留当前模型。"
        }
        "Models / relative roles" => "模型 / 相对角色",
        "[R] Refresh model catalog; no inference request is sent." => {
            "[R] 刷新模型目录；不会发送推理请求。"
        }
        "Included" => "已纳入",
        "Excluded" => "已排除",
        "unassigned" => "未指定角色",
        "economy" => "续航",
        "balanced" => "均衡",
        "capability" => "能力",
        "* includes every supported model. Choose a model number to change its role or eligibility." => {
            "* 允许所有受支持模型。选择模型序号可修改角色或是否纳入。"
        }
        "Model number or * (Enter returns)" => "模型序号或 *（回车返回）",
        "Model number, * or R (Enter returns)" => "模型序号、* 或 R（回车返回）",
        "Model catalog checked. Available entries may come from the last verified snapshot." => {
            "已查询模型目录；可用条目可能来自最近一次验证的快照。"
        }
        "0 Default role  1 Economy  2 Balanced  3 Capability  4 Toggle eligibility" => {
            "0 默认角色  1 续航  2 均衡  3 能力  4 切换是否纳入"
        }
        "Model setting number (Enter returns)" => "模型设置序号（回车返回）",
        "Keep at least one model, or use * to allow all." => {
            "请至少保留一个模型，或用 * 允许全部模型。"
        }
        "Configure Jev / Clef before enabling decision-service model selection." => {
            "启用服务决策前请先配置 Jev / Clef。"
        }
        "Model routing settings saved. New tasks read this policy; active tasks keep their admitted choice." => {
            "模型选择设置已保存。新任务读取此策略，进行中的任务保留已确定的模型。"
        }
        "Local preview only. No task was sent to a decision service and no inference was started." => {
            "仅在本地预览。没有发送任务给决策服务，也没有发起推理。"
        }
        "Model routing settings changed. Reload before saving." => {
            "模型选择设置已改变，请重载后再保存。"
        }
        _ => return None,
    })
}
