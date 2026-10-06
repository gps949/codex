//! Frozen model-policy changes enter the existing explicit confirmation flow.

use super::*;
use crate::native_account_view::routing::RoutingChange;
use crate::native_account_view::routing::RoutingPage;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingSource;

impl NativeMenuSession {
    pub(in crate::native_account_view) fn prepare_thread_model_automatic(
        &mut self,
        language: NativeAccountLanguage,
    ) {
        let return_page = MenuPage::Routing(RoutingPage::Mode);
        self.pending = Some(PendingOperation {
            operation: AccountManagerOperation::ThreadModelAutomatic,
            target: None, expected_identity: None,
            title: language.text("Return this thread to auto?", "此会话恢复自动选择？").into(),
            description: language.text("Release the manual model pin for subsequent tasks. Global Off/Preview/Automatic policy and the active task stay unchanged.\nNo Jev/Clef sharing consent, paid API use or reset credit is authorized. No inference request is sent.", "解除此会话后续任务的手动模型固定。全局关闭/预览/自动策略与当前任务不变。\n不会授权 Jev/Clef 数据分享、付费 API 或兑券；不发送推理请求。").into(),
            return_page, pending_reset: None,
        });
        self.return_page = return_page;
        self.page = MenuPage::Confirm;
    }

    pub(in crate::native_account_view) fn prepare_routing_text(
        &mut self,
        value: &str,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<bool> {
        if self.page != MenuPage::Routing(RoutingPage::CustomPreference) {
            return Ok(false);
        }
        let preference = value
            .parse::<u8>()
            .ok()
            .filter(|value| *value <= 100)
            .ok_or_else(|| {
                anyhow::anyhow!(language.text(
                    "Enter a whole number from 0 to 100",
                    "请输入 0 到 100 的整数"
                ))
            })?;
        self.prepare_routing(RoutingChange::Preference(preference), language)?;
        Ok(true)
    }

    pub(in crate::native_account_view) fn prepare_routing(
        &mut self,
        change: RoutingChange,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<()> {
        let view = self.inventory.routing.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Model routing settings could not be loaded. Reopen the menu.")
        })?;
        let mut config = view.config.clone();
        let return_page = match change {
            RoutingChange::Mode(mode) => {
                config.mode = mode;
                RoutingPage::Mode
            }
            RoutingChange::Source(source) => {
                config.source = source;
                config.send_task_description = source == ModelRoutingSource::DecisionService;
                RoutingPage::Source
            }
            RoutingChange::MainTasks => {
                config.main_tasks = !config.main_tasks;
                RoutingPage::Controls
            }
            RoutingChange::Subagents => {
                config.subagents = !config.subagents;
                RoutingPage::Controls
            }
            RoutingChange::Preference(preference) => {
                config.preference = preference;
                RoutingPage::Preference
            }
            RoutingChange::Effort(effort) => {
                config.max_effort = effort.as_str().into();
                RoutingPage::Effort
            }
            RoutingChange::LocalFallback => {
                config.local_fallback = !config.local_fallback;
                RoutingPage::Advanced
            }
        };
        let external = config.source == ModelRoutingSource::DecisionService
            && (config.mode != ModelRoutingMode::Off
                || matches!(
                    change,
                    RoutingChange::Source(ModelRoutingSource::DecisionService)
                ));
        if external {
            anyhow::ensure!(
                view.decision_service_ready,
                language.text(
                    "Configure Jev / Clef in the host manager first",
                    "请先在主机管理界面配置 Jev / Clef"
                )
            );
            config.send_task_description = true;
        }
        config.validate()?;
        let mut next = view.clone();
        next.config = config.clone();
        next.last_decision = None;
        let mut description = crate::native_account_view::routing::summary(&next, language);
        if external {
            description.push_str(language.text("\nConsent: share eligible task text (max 2048 bytes); separate fees may apply. No history, source files, account data or quota.", "\n同意发送符合条件任务的描述（最多 2048 字节），可能独立计费。不发送历史、源码、账号详情或额度。"));
        }
        description.push_str(language.text(
            "\nApplies to new tasks; manual model choices still win.",
            "\n新任务生效，手动指定模型仍优先。",
        ));
        let return_page = MenuPage::Routing(return_page);
        self.pending = Some(PendingOperation {
            operation: AccountManagerOperation::RoutingSave {
                config,
                expected_version: Some(view.user_config_version.clone()),
            },
            target: None,
            expected_identity: None,
            title: language
                .text(
                    if external {
                        "Allow task-text requests?"
                    } else {
                        "Save model policy?"
                    },
                    if external {
                        "同意发送任务描述？"
                    } else {
                        "保存模型策略？"
                    },
                )
                .into(),
            description,
            return_page,
            pending_reset: None,
        });
        self.return_page = return_page;
        self.page = MenuPage::Confirm;
        Ok(())
    }
}

#[cfg(test)]
#[path = "native_account_routing_actions_tests.rs"]
mod tests;
