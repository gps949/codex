//! Conservative task routing using explicit catalog roles rather than inferred prices.

use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingTaskComplexity {
    Simple,
    Standard,
    Demanding,
    HighStakes,
    Unknown,
}

/// Relative policy roles; these do not measure capability or subscription consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingModelRole {
    Economy,
    Balanced,
    Capability,
}

#[derive(Debug, Clone)]
pub struct RoutingCandidate {
    pub model: ModelInfo,
    pub role: RoutingModelRole,
}

pub struct RoutingRequest<'a> {
    pub task: &'a str,
    pub current_model: &'a str,
    pub current_effort: Option<ReasoningEffort>,
    pub preference: u8,
    pub candidates: &'a [RoutingCandidate],
    pub allowed_models: &'a [String],
    pub required_context_tokens: i64,
    pub requires_images: bool,
    pub max_effort: ReasoningEffort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingSelection {
    pub model: String,
    pub effort: Option<ReasoningEffort>,
    pub reason: String,
}

/// Only explicit task evidence enables local routing; ambiguity preserves the current model.
pub fn classify_task_locally(task: &str) -> RoutingTaskComplexity {
    if task.trim().is_empty() || task.len() > 2048 {
        return RoutingTaskComplexity::Unknown;
    }
    let text = task
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_punctuation() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let text = format!(
        " {} ",
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    for (complexity, terms) in [
        (
            RoutingTaskComplexity::HighStakes,
            &[
                " security ",
                " authentication ",
                " authorization ",
                " credentials ",
                " production ",
                " payment ",
                " payments ",
                " financial ",
                " medical ",
                " legal ",
                " delete database ",
                " data loss ",
                "安全",
                "认证",
                "鉴权",
                "权限",
                "凭据",
                "生产",
                "支付",
                "金融",
                "医疗",
                "法律",
                "删库",
            ][..],
        ),
        (
            RoutingTaskComplexity::Demanding,
            &[
                " architecture ",
                " architectural ",
                " distributed ",
                " concurrency ",
                " concurrent ",
                " race ",
                " root cause ",
                " migration ",
                " refactor ",
                " performance ",
                " cross module ",
                " cross platform ",
                "架构",
                "分布式",
                "并发",
                "竞态",
                "根因",
                "迁移",
                "重构",
                "性能",
                "跨模块",
                "跨平台",
            ][..],
        ),
        (
            RoutingTaskComplexity::Standard,
            &[
                " implement ",
                " implementation ",
                " feature ",
                " bug ",
                " fix ",
                " test ",
                " tests ",
                " review ",
                " analyze ",
                " investigate ",
                " explain ",
                " plan ",
                "实现",
                "功能",
                "修复",
                "测试",
                "评审",
                "审查",
                "分析",
                "调查",
                "解释",
                "规划",
            ][..],
        ),
        (
            RoutingTaskComplexity::Simple,
            &[
                " translate ",
                " translation ",
                " rephrase ",
                " rewrite a sentence ",
                " format this ",
                " simple question ",
                " short answer ",
                " hello ",
                "翻译",
                "改写句子",
                "润色这句",
                "格式化",
                "一句话",
                "简单问题",
            ][..],
        ),
    ] {
        if complexity == RoutingTaskComplexity::Standard
            && task.len() <= 384
            && ![
                " implement ",
                " feature ",
                " bug ",
                " investigate ",
                "实现",
                "功能",
                "故障",
                "调查",
            ]
            .iter()
            .any(|term| text.contains(term))
            && (text.contains(" rename ")
                && text.contains(" variable ")
                && text.contains(" single file ")
                || text.contains(" typo ")
                    && (text.contains(" single line ") || text.contains(" one line "))
                || text.contains("变量") && text.contains("重命名") && text.contains("单文件")
                || (text.contains("错别字") || text.contains("拼写")) && text.contains("一行"))
        {
            return RoutingTaskComplexity::Simple;
        }
        if terms.iter().any(|term| text.contains(*term)) {
            return complexity;
        }
    }
    RoutingTaskComplexity::Unknown
}

/// Known exact catalog descriptions provide roles, without extrapolating model-name prefixes.
pub fn default_routing_role(info: &ModelInfo) -> Option<RoutingModelRole> {
    if !complete_model_metadata(info) {
        return None;
    }
    match (info.slug.as_str(), info.description.as_deref()) {
        ("gpt-6-luna", Some("Fast and affordable model for easier tasks."))
        | ("gpt-5.6-luna", Some("Older fast and efficient model.")) => {
            Some(RoutingModelRole::Economy)
        }
        ("gpt-6.1-sol", Some("Latest workhorse model for coding and everyday work."))
        | ("gpt-6-sol", Some("Previous generation workhorse model."))
        | ("gpt-5.6-sol", Some("Older generation workhorse model."))
        | ("gpt-5.6-terra", Some("Older balanced model for straightforward work.")) => {
            Some(RoutingModelRole::Balanced)
        }
        ("gpt-6-astra", Some("Frontier intelligence for the most demanding work.")) => {
            Some(RoutingModelRole::Capability)
        }
        _ => None,
    }
}

/// Chooses only an exact, compatible candidate. Caller admission still checks authorization.
pub fn choose_task_model(
    request: &RoutingRequest<'_>,
    complexity: RoutingTaskComplexity,
) -> Option<RoutingSelection> {
    if request.task.trim().is_empty()
        || request.task.len() > 2048
        || request.preference > 100
        || request.required_context_tokens < 0
        || request.candidates.is_empty()
        || complexity == RoutingTaskComplexity::Unknown
    {
        return None;
    }
    let mut names = HashSet::new();
    if request
        .candidates
        .iter()
        .any(|candidate| !names.insert(&candidate.model.slug))
    {
        return None;
    }
    let current = request
        .candidates
        .iter()
        .find(|candidate| candidate.model.slug == request.current_model)?;
    if !complete_model_metadata(&current.model) {
        return None;
    }
    let local = classify_task_locally(request.task);
    let complexity = match (local, complexity) {
        (RoutingTaskComplexity::HighStakes, _) => RoutingTaskComplexity::HighStakes,
        (
            RoutingTaskComplexity::Demanding,
            RoutingTaskComplexity::Simple | RoutingTaskComplexity::Standard,
        ) => RoutingTaskComplexity::Demanding,
        (RoutingTaskComplexity::Standard, RoutingTaskComplexity::Simple) => {
            RoutingTaskComplexity::Standard
        }
        (_, classified) => classified,
    };
    let (floor, desired_effort, label) = match complexity {
        RoutingTaskComplexity::Simple => (0, 2, "simple"),
        RoutingTaskComplexity::Standard => (1, 3, "standard"),
        RoutingTaskComplexity::Demanding => (1, 4, "demanding"),
        RoutingTaskComplexity::HighStakes => (2, 4, "high_stakes"),
        RoutingTaskComplexity::Unknown => return None,
    };
    let preferred = match request.preference {
        0..=33 => 0,
        34..=66 => 1,
        67..=100 => 2,
        _ => return None,
    };
    let preferred = preferred.max(floor);
    let maximum = match &request.max_effort {
        ReasoningEffort::Ultra => 6,
        effort => effort_rank(effort)?,
    };
    // Retain exact current metadata independently of its allowlist eligibility. Bound the
    // compatible selection set only after filtering; the extra entry detects overflow without
    // silently truncating either the current model or a later allowed target.
    let candidates = request
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            complete_model_metadata(&candidate.model)
                && (request.allowed_models.is_empty()
                    || request.allowed_models.contains(&candidate.model.slug))
                && role_rank(candidate.role) >= floor
                && candidate
                    .model
                    .usable_context_window()
                    .is_some_and(|window| window >= request.required_context_tokens)
                && (!request.requires_images
                    || candidate
                        .model
                        .input_modalities
                        .contains(&InputModality::Image))
        })
        .take(/*n*/ 33)
        .collect::<Vec<_>>();
    if candidates.len() > 32 {
        return None;
    }
    let selected = candidates
        .into_iter()
        .filter_map(|(index, candidate)| {
            let effort = candidate
                .model
                .supported_reasoning_levels
                .iter()
                .filter_map(|level| effort_rank(&level.effort).map(|rank| (rank, &level.effort)))
                .filter(|(rank, _)| *rank >= desired_effort && *rank <= maximum)
                .min_by_key(|(rank, _)| *rank)?;
            let role = role_rank(candidate.role);
            Some((
                (
                    role.abs_diff(preferred),
                    candidate.model.slug != request.current_model,
                    index,
                ),
                candidate,
                effort.1.clone(),
            ))
        })
        .min_by_key(|(key, _, _)| *key)?;
    let role = match selected.1.role {
        RoutingModelRole::Economy => "economy",
        RoutingModelRole::Balanced => "balanced",
        RoutingModelRole::Capability => "capability",
    };
    Some(RoutingSelection {
        model: selected.1.model.slug.clone(),
        effort: Some(selected.2),
        reason: format!("Task complexity: {label}; model role: {role}."),
    })
}

fn complete_model_metadata(info: &ModelInfo) -> bool {
    !info.used_fallback_model_metadata
        && !info.slug.is_empty()
        && info.slug.len() <= 128
        && !info.slug.chars().any(char::is_control)
        && info.visibility == ModelVisibility::List
        && info.model_specialty.is_none()
        && info.description.as_ref().is_some_and(|description| {
            !description.trim().is_empty()
                && !["retired", "deprecated", "legacy"]
                    .iter()
                    .any(|marker| description.to_ascii_lowercase().contains(*marker))
        })
        && info
            .resolved_context_window()
            .is_some_and(|window| window > 0)
        && info.max_context_window.is_none_or(|maximum| {
            maximum > 0 && info.context_window.is_none_or(|window| window <= maximum)
        })
        && (1..=100).contains(&info.effective_context_window_percent)
        && info.input_modalities.contains(&InputModality::Text)
        && !info.supported_reasoning_levels.is_empty()
        && info
            .default_reasoning_level
            .as_ref()
            .is_some_and(|default| {
                info.supported_reasoning_levels
                    .iter()
                    .any(|level| &level.effort == default)
            })
}

fn role_rank(role: RoutingModelRole) -> u8 {
    match role {
        RoutingModelRole::Economy => 0,
        RoutingModelRole::Balanced => 1,
        RoutingModelRole::Capability => 2,
    }
}

fn effort_rank(effort: &ReasoningEffort) -> Option<u8> {
    match effort {
        ReasoningEffort::None => Some(0),
        ReasoningEffort::Minimal => Some(1),
        ReasoningEffort::Low => Some(2),
        ReasoningEffort::Medium => Some(3),
        ReasoningEffort::High => Some(4),
        ReasoningEffort::XHigh => Some(5),
        ReasoningEffort::Max => Some(6),
        ReasoningEffort::Ultra | ReasoningEffort::Persistent | ReasoningEffort::Custom(_) => None,
    }
}

#[cfg(test)]
#[path = "model_routing_tests.rs"]
mod tests;
