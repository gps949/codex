//! Selects an unpinned child once, before startup and service-tier validation.

use crate::config::Config;
use crate::context_manager::estimate_item_token_count;
use crate::session::model_selection::has_images;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::task_model_routing::RoutingScope;
use crate::task_model_routing::TaskRoutingDecision;
use crate::task_model_routing::TaskRoutingInput;
use crate::task_model_routing::decide_task_model;
use crate::thread_rollout_truncation::truncate_rollout_to_last_n_fork_turns;
use codex_history::RolloutItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

pub(super) async fn route(
    session: &Session,
    step: &StepContext,
    config: &mut Config,
    task: &str,
    history_last_n_turns: Option<usize>,
) -> Option<TaskRoutingDecision> {
    if task.trim().is_empty() || task.len() > 2048 {
        return None;
    }
    let inherited = if let Some(turns) = history_last_n_turns {
        let history = session
            .clone_history()
            .await
            .into_annotated_items()
            .into_iter()
            .map(RolloutItem::ResponseItem)
            .collect();
        truncate_rollout_to_last_n_fork_turns(history, turns)
            .into_iter()
            .filter_map(|item| {
                if let RolloutItem::ResponseItem(item) = item {
                    Some(item)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if inherited.iter().any(|item| {
        matches!(&item.item, ResponseItem::Message { content, .. }
        if content.iter().any(|part| matches!(part, ContentItem::InputAudio { .. })))
    }) {
        return None;
    }
    let requires_images = has_images(inherited.iter().map(|item| &item.item));
    let required_context_tokens = inherited
        .iter()
        .map(|item| estimate_item_token_count(&item.item))
        .fold(0i64, i64::saturating_add)
        .saturating_add(i64::try_from(task.len()).unwrap_or(i64::MAX))
        .saturating_add(
            i64::try_from(config.base_instructions.as_ref().map_or(0, String::len))
                .unwrap_or(i64::MAX),
        )
        .saturating_add(8192);
    let retained_service_tier = session.services.agent_control.service_tier();
    let decision = decide_task_model(
        config,
        session.services.models_manager.as_ref(),
        TaskRoutingInput {
            task,
            current_model: config
                .model
                .as_deref()
                .unwrap_or(&step.settings.model_info.slug),
            current_effort: config.model_reasoning_effort.clone(),
            service_tier: retained_service_tier.as_deref(),
            required_context_tokens,
            requires_images,
            scope: RoutingScope::Subagent,
        },
    )
    .await?;
    let requirements = config.config_layer_stack.requirements();
    if requirements.auto_review_required_for_model(&decision.selection.model)
        != requirements.auto_review_required_for_model(
            config
                .model
                .as_deref()
                .unwrap_or(&step.settings.model_info.slug),
        )
    {
        return None;
    }
    let changed = config.model.as_deref() != Some(&decision.selection.model)
        || config.model_reasoning_effort != decision.selection.effort;
    let action = if decision.apply {
        "selected"
    } else {
        "suggests"
    };
    let effort = decision
        .selection
        .effort
        .as_ref()
        .map_or("default".to_string(), ToString::to_string);
    if !decision.apply || changed {
        session
            .send_event_raw(Event {
                id: step.turn.sub_id.clone(),
                msg: EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Subagent model routing {action} {} ({effort}) using {}.",
                        decision.selection.model, decision.source
                    ),
                }),
            })
            .await;
    }
    if decision.apply {
        config.model = Some(decision.selection.model.clone());
        config.model_reasoning_effort = decision.selection.effort.clone();
    }
    Some(decision)
}
