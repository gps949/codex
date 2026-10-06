//! Keeps manual model choices separate from client echoes and task recommendations.

use super::session::Session;
use super::session::SessionSettingsUpdate;
use super::step_settings::StepSettings;
use super::step_settings::StepSettingsUpdate;
use crate::config::Config;
use crate::task_model_routing::RoutingScope;
use crate::task_model_routing::TaskRoutingDecision;
use crate::task_model_routing::TaskRoutingInput;
use crate::task_model_routing::decide_task_model;
use crate::task_model_routing::estimate_fresh_input_tokens;
use crate::task_model_routing::retained_media_requirements;
use codex_history::InitialHistory;
use codex_history::RolloutItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelSelectionIntent;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::user_input::UserInput;

pub(super) fn initial_intent(config: &Config, history: &InitialHistory) -> ModelSelectionIntent {
    if let Some(intent @ (ModelSelectionIntent::Explicit | ModelSelectionIntent::Automatic)) =
        config.model_selection_override
    {
        return intent;
    }
    if let InitialHistory::Resumed(resumed) = history
        && let Some(intent) = resumed.history.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                if event.thread_id == Some(resumed.conversation_id) =>
            {
                event.thread_settings.model_selection_intent
            }
            _ => None,
        })
    {
        return intent;
    }
    if config.model_selection_is_explicit {
        ModelSelectionIntent::Explicit
    } else {
        ModelSelectionIntent::Automatic
    }
}

pub(super) fn prepare_update(
    current_intent: ModelSelectionIntent,
    current: &StepSettings,
    updates: &SessionSettingsUpdate,
) -> (StepSettingsUpdate, ModelSelectionIntent) {
    let mut step = updates.step_settings.clone();
    let intent = match updates.model_selection_intent {
        Some(ModelSelectionIntent::FollowThread) => {
            step.model = None;
            step.effort = None;
            if let Some(mode) = &mut step.collaboration_mode {
                mode.settings
                    .model
                    .clone_from(&current.collaboration_mode.settings.model);
                mode.settings
                    .reasoning_effort
                    .clone_from(&current.collaboration_mode.settings.reasoning_effort);
            }
            current_intent
        }
        Some(ModelSelectionIntent::Explicit) => ModelSelectionIntent::Explicit,
        Some(ModelSelectionIntent::Automatic) => ModelSelectionIntent::Automatic,
        None if step.model.is_some()
            || step.effort.is_some()
            || step.collaboration_mode.is_some() =>
        {
            ModelSelectionIntent::Explicit
        }
        None => current_intent,
    };
    (step, intent)
}

/// Legacy turn clients echo their visible selection. After automatic selection is restored,
/// exact echoes retain that policy; deliberate changes and standalone updates remain explicit.
pub(super) async fn retain_legacy_turn_echo(
    session: &Session,
    mut overrides: ThreadSettingsOverrides,
) -> ThreadSettingsOverrides {
    if overrides.model_selection_intent.is_some()
        || overrides.model.is_none()
            && overrides.effort.is_none()
            && overrides.collaboration_mode.is_none()
    {
        return overrides;
    }
    let current = session.thread_settings_snapshot().await;
    if current.model_selection_intent == Some(ModelSelectionIntent::Automatic)
        && overrides
            .model
            .as_ref()
            .is_none_or(|model| model == &current.model)
        && overrides
            .effort
            .as_ref()
            .is_none_or(|effort| effort == &current.reasoning_effort)
        && overrides.collaboration_mode.as_ref().is_none_or(|mode| {
            mode.settings.model == current.model
                && mode.settings.reasoning_effort == current.reasoning_effort
        })
    {
        overrides.model_selection_intent = Some(ModelSelectionIntent::FollowThread);
    }
    overrides
}

/// Called only after new user-turn admission, before committing its settings.
pub(super) async fn route_root_task(
    session: &Session,
    input: &TurnInput,
    updates: &mut SessionSettingsUpdate,
    submission_id: &str,
) -> Option<TaskRoutingDecision> {
    let TurnInput::UserInput { content, .. } = input else {
        return None;
    };
    if content.is_empty()
        || content.iter().any(|input| {
            matches!(
                input,
                UserInput::Audio { .. } | UserInput::LocalAudio { .. }
            )
        })
    {
        return None;
    }
    let Ok(snapshot) = session.preview_settings(updates).await else {
        return None;
    };
    if snapshot.session_source.is_non_root_agent()
        || snapshot.model_selection_intent == Some(ModelSelectionIntent::Explicit)
    {
        return None;
    }
    let mut task = String::new();
    for input in content {
        if let UserInput::Text { text, .. } = input {
            let separator_len = usize::from(!task.is_empty());
            if task
                .len()
                .saturating_add(text.len())
                .saturating_add(separator_len)
                > 2048
            {
                return None;
            }
            if !task.is_empty() {
                task.push('\n');
            }
            task.push_str(text);
        }
    }
    if task.trim().is_empty() {
        return None;
    }
    let history = session.clone_history().await;
    let media = retained_media_requirements(history.raw_items());
    if media.audio {
        return None;
    }
    let requires_images = media.images
        || content.iter().any(|input| {
            matches!(
                input,
                UserInput::Image { .. } | UserInput::LocalImage { .. }
            )
        });
    let base = session.get_base_instructions().await;
    let history_tokens = history
        .estimate_token_count_with_base_instructions(&base)
        .unwrap_or(i64::MAX)
        .max(session.get_total_token_usage().await);
    // Retained token usage is context, not billing. Fresh input estimates keep text and media
    // margins separate; reserve additional room for model-owned instructions and tool schemas.
    let required_context_tokens = history_tokens
        .saturating_add(estimate_fresh_input_tokens(content))
        .saturating_add(8192);
    let config = session.get_config().await;
    let decision = decide_task_model(
        &config,
        session.services.models_manager.as_ref(),
        TaskRoutingInput {
            task: &task,
            current_model: &snapshot.model,
            current_effort: snapshot.reasoning_effort.clone(),
            service_tier: updates
                .service_tier_for_turn
                .as_deref()
                .or(snapshot.service_tier.as_deref()),
            required_context_tokens,
            requires_images,
            scope: RoutingScope::Main,
        },
    )
    .await?;
    let mut candidate = updates.clone();
    candidate.model_selection_intent = Some(ModelSelectionIntent::Automatic);
    candidate.step_settings.model = None;
    candidate.step_settings.effort = None;
    candidate.step_settings.collaboration_mode = Some(snapshot.collaboration_mode.with_updates(
        Some(decision.selection.model.clone()),
        Some(decision.selection.effort.clone()),
        /*developer_instructions*/ None,
    ));
    let Ok(checked) = session.preview_settings(&candidate).await else {
        return None;
    };
    if checked.approval_policy != snapshot.approval_policy
        || checked.approvals_reviewer != snapshot.approvals_reviewer
        || checked.permission_profile != snapshot.permission_profile
    {
        return None;
    }
    let changed = decision.selection.model != snapshot.model
        || decision.selection.effort != snapshot.reasoning_effort;
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
                id: submission_id.to_string(),
                msg: EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Model routing {action} {} ({effort}) using {}.",
                        decision.selection.model, decision.source
                    ),
                }),
            })
            .await;
    }
    if decision.apply {
        *updates = candidate;
    }
    Some(decision)
}
