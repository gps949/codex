//! Short native model-policy pages; task-text sharing requires a separate confirmation.

use super::*;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingPage {
    Mode,
    Controls,
    Source,
    Preference,
    CustomPreference,
    Advanced,
    Effort,
    Models(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl RoutingEffort {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingChange {
    Mode(ModelRoutingMode),
    Source(ModelRoutingSource),
    MainTasks,
    Subagents,
    Preference(u8),
    Effort(RoutingEffort),
    LocalFallback,
}

pub(super) fn mode_name(mode: ModelRoutingMode, language: NativeAccountLanguage) -> &'static str {
    match mode {
        ModelRoutingMode::Off => language.text("Off", "关闭"),
        ModelRoutingMode::Preview => language.text("Preview", "仅预览"),
        ModelRoutingMode::Automatic => language.text("Automatic", "自动选择"),
    }
}

pub(super) fn source_name(
    source: ModelRoutingSource,
    language: NativeAccountLanguage,
) -> &'static str {
    match source {
        ModelRoutingSource::Local => language.text("Local rules", "本地规则"),
        ModelRoutingSource::DecisionService => language.text("Jev / Clef", "Jev / Clef"),
    }
}

pub(super) fn summary(
    view: &crate::account_management::ModelRoutingView,
    language: NativeAccountLanguage,
) -> String {
    let config = &view.config;
    let mut summary = format!(
        "{}: {} · {}\n{}: {} · {}: {}\n{}: {}/100 · {}: {}",
        language.text("Saved", "已保存"),
        mode_name(config.mode, language),
        source_name(config.source, language),
        language.text("Main", "主任务"),
        on_off(config.main_tasks, language),
        language.text("Subagents", "子代理"),
        on_off(config.subagents, language),
        language.text("Balance", "倾向"),
        config.preference,
        language.text("Max effort", "最高思考"),
        config.max_effort
    );
    if view.overridden {
        summary.push_str(&format!(
            "\n{}: {}",
            language.text("Effective override", "优先配置实际模式"),
            mode_name(view.effective_config.mode, language)
        ));
    }
    if let Some(last) = &view.last_decision
        && let Some(model) = last["model"].as_str()
    {
        summary.push_str(&format!(
            "\n{}: {}",
            if last["applied"].as_bool() == Some(true) || last["status"].as_str() == Some("applied")
            {
                language.text("Last applied", "最近已应用")
            } else {
                language.text("Last proposed", "最近建议")
            },
            bounded_text(model, /*max_chars*/ 96)
        ));
    }
    summary
}

fn on_off(enabled: bool, language: NativeAccountLanguage) -> &'static str {
    if enabled {
        language.text("On", "开启")
    } else {
        language.text("Off", "关闭")
    }
}

impl FrozenAccountInventory {
    pub(super) fn routing_question(
        &self,
        page: RoutingPage,
        language: NativeAccountLanguage,
    ) -> MenuQuestion {
        let nav = |en, zh, description: String, page| {
            choice(
                language.text(en, zh),
                description,
                MenuAction::Page(MenuPage::Routing(page)),
            )
        };
        let action = |en, zh, description: String, change| {
            choice(
                language.text(en, zh),
                description,
                MenuAction::Prepare(MenuOperation::Routing(change)),
            )
        };
        let back = |page| nav("Back", "返回", String::new(), page);
        let Some(view) = &self.routing else {
            return MenuQuestion::new(
                language.text("Model settings unavailable", "模型设置不可用"),
                vec![
                    choice(
                        language.text("Use auto for this thread", "此会话恢复自动选择"),
                        language.text(
                            "Release the manual model pin; global policy stays unchanged",
                            "解除手动模型固定；全局策略不变",
                        ),
                        MenuAction::Prepare(MenuOperation::ThreadModelAutomatic),
                    ),
                    choice(
                        language.text("Reload inventory", "重载设置"),
                        language.text("Retry loading model policy", "重新读取模型策略"),
                        MenuAction::Execute(MenuOperation::Reload),
                    ),
                    choice(
                        language.text("Back", "返回"),
                        "",
                        MenuAction::Page(MenuPage::Home),
                    ),
                ],
            );
        };
        let config = &view.config;
        match page {
            RoutingPage::Mode => MenuQuestion::new(
                language.text("Model selection", "模型选择"),
                vec![
                    action(
                        "Off",
                        "关闭",
                        summary(view, language),
                        RoutingChange::Mode(ModelRoutingMode::Off),
                    ),
                    action(
                        "Preview only",
                        "仅预览",
                        language
                            .text(
                                "Record suggestions; keep the current model",
                                "仅记录建议，保留当前模型",
                            )
                            .into(),
                        RoutingChange::Mode(ModelRoutingMode::Preview),
                    ),
                    action(
                        "Automatic",
                        "自动选择",
                        language
                            .text(
                                "Apply valid choices on new tasks; manual choices still win",
                                "新任务应用有效选择，手动指定仍优先",
                            )
                            .into(),
                        RoutingChange::Mode(ModelRoutingMode::Automatic),
                    ),
                    choice(
                        language.text("Use auto for this thread", "此会话恢复自动选择"),
                        language.text(
                            "Release the manual model pin; global policy stays unchanged",
                            "解除手动模型固定；全局策略不变",
                        ),
                        MenuAction::Prepare(MenuOperation::ThreadModelAutomatic),
                    ),
                    nav(
                        "Policy controls",
                        "策略控制",
                        format!(
                            "{} · {}/100",
                            source_name(config.source, language),
                            config.preference
                        ),
                        RoutingPage::Controls,
                    ),
                    choice(
                        language.text("Back", "返回"),
                        "",
                        MenuAction::Page(MenuPage::Home),
                    ),
                ],
            ),
            RoutingPage::Controls => MenuQuestion::new(
                language.text("Model policy", "模型策略"),
                vec![
                    nav(
                        "Decision source",
                        "决策来源",
                        source_name(config.source, language).into(),
                        RoutingPage::Source,
                    ),
                    action(
                        "Main tasks",
                        "主任务",
                        format!(
                            "{} → {}",
                            on_off(config.main_tasks, language),
                            on_off(!config.main_tasks, language)
                        ),
                        RoutingChange::MainTasks,
                    ),
                    action(
                        "Subagents",
                        "子代理",
                        format!(
                            "{} → {}",
                            on_off(config.subagents, language),
                            on_off(!config.subagents, language)
                        ),
                        RoutingChange::Subagents,
                    ),
                    nav(
                        "Endurance / capability",
                        "续航 / 能力倾向",
                        format!(
                            "{}/100 · {}",
                            config.preference,
                            language.text("0 endurance · 100 capability", "0 续航 · 100 能力")
                        ),
                        RoutingPage::Preference,
                    ),
                    nav(
                        "Effort and models",
                        "思考强度与模型",
                        config.max_effort.clone(),
                        RoutingPage::Advanced,
                    ),
                    back(RoutingPage::Mode),
                ],
            ),
            RoutingPage::Source => {
                let mut choices = vec![action(
                    "Local rules",
                    "本地规则",
                    language
                        .text(
                            "No external task-text request; revoke sharing consent",
                            "不向外部发送任务描述；撤回分享同意",
                        )
                        .into(),
                    RoutingChange::Source(ModelRoutingSource::Local),
                )];
                if view.decision_service_ready {
                    choices.push(action(
                        "Jev / Clef",
                        "Jev / Clef",
                        language
                            .text(
                                "Requires task-text and separate-fee confirmation",
                                "需要确认发送任务描述与独立费用",
                            )
                            .into(),
                        RoutingChange::Source(ModelRoutingSource::DecisionService),
                    ));
                } else {
                    choices[0].description.push_str(language.text(
                        "\nJev / Clef is not configured. Set it up in the WebUI or terminal manager's Jev / Clef section.",
                        "\n尚未配置 Jev / Clef。请先在 WebUI 或终端管理器的 Jev / Clef 页面设置服务。",
                    ));
                }
                choices.push(back(RoutingPage::Controls));
                MenuQuestion::new(language.text("Decision source", "决策来源"), choices)
            }
            RoutingPage::Preference => {
                let mut choices = [0, 25, 50, 75, 100]
                    .into_iter()
                    .map(|value| {
                        choice(
                            &format!("{value}/100"),
                            match value {
                                0 => language.text("Favor endurance", "优先续航"),
                                25 => language.text("Lean toward endurance", "偏向续航"),
                                50 => language
                                    .text("Balance endurance and capability", "兼顾续航与能力"),
                                75 => language.text("Lean toward capability", "偏向能力"),
                                100 => language.text("Favor capability", "优先能力"),
                                _ => unreachable!("listed preference"),
                            },
                            MenuAction::Prepare(MenuOperation::Routing(RoutingChange::Preference(
                                value,
                            ))),
                        )
                    })
                    .collect::<Vec<_>>();
                choices.push(nav(
                    "Custom value",
                    "自定义数值",
                    language
                        .text("Enter a whole number from 0 to 100", "输入 0 到 100 的整数")
                        .into(),
                    RoutingPage::CustomPreference,
                ));
                choices.push(back(RoutingPage::Controls));
                MenuQuestion::new(
                    language.text("Endurance / capability", "续航 / 能力倾向"),
                    choices,
                )
            }
            RoutingPage::CustomPreference => MenuQuestion {
                text: language.text("Balance: 0 to 100", "倾向：0 到 100").into(),
                choices: vec![],
                free_text: true,
            },
            RoutingPage::Advanced => MenuQuestion::new(
                language.text("Model options", "模型选项"),
                vec![
                    nav(
                        "Highest effort",
                        "最高思考强度",
                        config.max_effort.clone(),
                        RoutingPage::Effort,
                    ),
                    nav(
                        "Available models",
                        "可用模型",
                        language
                            .text(
                                "Review exact candidates and relative roles",
                                "查看准确候选模型及相对角色",
                            )
                            .into(),
                        RoutingPage::Models(0),
                    ),
                    action(
                        "Local fallback",
                        "本地规则兜底",
                        format!(
                            "{} → {}\n{}",
                            on_off(config.local_fallback, language),
                            on_off(!config.local_fallback, language),
                            language.text(
                                "Use local rules if the decision service fails",
                                "决策服务失败时改用本地规则"
                            )
                        ),
                        RoutingChange::LocalFallback,
                    ),
                    back(RoutingPage::Controls),
                ],
            ),
            RoutingPage::Effort => {
                let mut choices = [
                    RoutingEffort::Minimal,
                    RoutingEffort::Low,
                    RoutingEffort::Medium,
                    RoutingEffort::High,
                    RoutingEffort::Xhigh,
                    RoutingEffort::Max,
                ]
                .into_iter()
                .map(|effort| {
                    choice(
                        effort.as_str(),
                        language.text(
                            "Only supported efforts are selected; Ultra stays manual",
                            "仅选择模型支持的思考强度；Ultra 仍需手动",
                        ),
                        MenuAction::Prepare(MenuOperation::Routing(RoutingChange::Effort(effort))),
                    )
                })
                .collect::<Vec<_>>();
                choices.push(back(RoutingPage::Advanced));
                MenuQuestion::new(language.text("Highest effort", "最高思考强度"), choices)
            }
            RoutingPage::Models(page) => {
                let page = page.min(view.models.len().div_ceil(3).saturating_sub(1));
                let mut choices = view
                    .models
                    .iter()
                    .skip(page * 3)
                    .take(3)
                    .map(|model| {
                        let role = match model.role.as_deref() {
                            Some("economy") => language.text("Economy", "续航"),
                            Some("balanced") => language.text("Balanced", "均衡"),
                            Some("capability") => language.text("Capability", "能力"),
                            _ => language.text("Role not assigned", "尚未指定角色"),
                        };
                        let included = config.allowed_models.is_empty()
                            || config.allowed_models.contains(&model.model);
                        choice(
                            &model.model,
                            format!(
                                "{role} · {}\n{}",
                                if included {
                                    language.text("Included", "已纳入")
                                } else {
                                    language.text("Excluded", "已排除")
                                },
                                language.text(
                                    "Edit roles and eligibility in WebUI or terminal manager",
                                    "可在 WebUI 或终端管理器修改角色与是否纳入"
                                )
                            ),
                            MenuAction::Page(MenuPage::Routing(RoutingPage::Models(page))),
                        )
                    })
                    .collect::<Vec<_>>();
                if choices.is_empty() {
                    choices.push(nav(
                        "No eligible models",
                        "暂无符合条件的模型",
                        language
                            .text(
                                "Keep the current model until catalog candidates are available",
                                "候选目录可用前保留当前模型",
                            )
                            .into(),
                        RoutingPage::Models(0),
                    ));
                }
                if view.models.len() > 3 {
                    choices.push(nav(
                        "Next models",
                        "下一页模型",
                        String::new(),
                        RoutingPage::Models((page + 1) % view.models.len().div_ceil(3)),
                    ));
                }
                choices.push(choice(
                    language.text("Refresh model catalog", "刷新模型目录"),
                    language.text(
                        "Read the latest catalog; no inference request is sent",
                        "读取最新目录；不会发送推理请求",
                    ),
                    MenuAction::Execute(MenuOperation::RoutingRefreshModels),
                ));
                choices.push(back(RoutingPage::Advanced));
                MenuQuestion::new(language.text("Available models", "可用模型"), choices)
            }
        }
    }
}
