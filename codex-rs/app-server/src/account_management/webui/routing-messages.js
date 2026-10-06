"use strict";

(() => {
  const chinese = {
    "Automatic model selection": "自动选择模型",
    "Off": "关闭",
    "Preview only": "仅建议",
    "Automatic": "自动应用",
    "Local rules": "本地规则",
    "Configured Jev / Clef": "已配置的 Jev / Clef",
    "Selection mode": "选择模式",
    "Decision source": "决策来源",
    "New main tasks": "新的主任务",
    "New subagents": "新的子代理",
    "Longer use": "更持久",
    "More capability": "更强能力",
    "Preference": "决策倾向",
    "Manual model choices take priority. Active tasks keep their admitted model.":
      "手动选择模型优先。正在执行的任务保留开始时的模型。",
    "Model limits and service settings": "模型范围与服务设置",
    "Highest automatic effort": "自动选择的最高思考强度",
    "Send a short task description to the configured service":
      "将简短任务说明发送到已配置的服务",
    "Only task text, up to 2048 bytes. Service charges may apply.":
      "仅发送任务文字，最多 2048 字节。服务商可能收费。",
    "Use local rules if the service fails": "服务失败时使用本地规则",
    "Configure Jev / Clef": "配置 Jev / Clef",
    "Choose one service in Decision assistance first.":
      "请先在决策辅助中配置一种服务。",
    "Available models": "可选模型",
    "Refresh model catalog": "刷新模型目录",
    "Economy": "续航型",
    "Balanced": "均衡型",
    "Capability": "能力型",
    "Unassigned": "未分配",
    "Select at least one model.": "请至少选择一个模型。",
    "Use catalog role": "使用默认角色",
    "Roles express your preference, not measured quota costs.":
      "角色表示你的偏好，不代表实测额度消耗。",
    "No verified models are available. Sign in and refresh the model catalog.":
      "暂无已确认的模型，请登录并刷新模型目录。",
    "Save selection policy": "保存选择策略",
    "Local simulation": "本地模拟",
    "Describe a task": "描述一个任务",
    "Simulate selection": "模拟选择",
    "Simulation uses local rules and sends no external request.":
      "模拟仅使用本地规则，不发送外部请求。",
    "Keep current model; no compatible selection.":
      "保留当前模型；未找到兼容的选择。",
    "Applied": "已应用",
    "Suggested": "建议",
    "No decision recorded yet.": "尚未记录选择结果。",
    "Last decision": "最近一次选择",
    "Settings changed elsewhere. Reload or save to check your version.":
      "设置已在其他地方改变。可重载，或保存时检查版本。",
    "Reload policy": "重载策略",
    "Higher-priority settings override this saved policy.":
      "此处保存的策略被更高优先级设置覆盖。",
    "Model routing settings changed. Reload before saving.":
      "模型选择设置已改变，请重载后保存。",
    "Configure a decision service token before enabling task routing.":
      "启用任务选模前，请先配置决策服务令牌。",
    "Decision-service model routing requires separate consent to send the task description":
      "使用决策服务选模需要单独同意发送任务说明",
    "{model} · effort {effort}": "{model} · 思考强度 {effort}",
  };
  function t(message, values = {}) {
    const template =
      window.AccountManagerMessages.language() === "zh-CN" &&
      Object.hasOwn(chinese, message)
        ? chinese[message]
        : String(message);
    return template.replace(/\{(\w+)\}/g, (match, key) =>
      Object.hasOwn(values, key) ? String(values[key]) : match,
    );
  }
  window.AccountManagerRoutingMessages = Object.freeze({ t });
})();
