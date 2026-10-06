use super::*;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelVisibility;
use pretty_assertions::assert_eq;

fn model(slug: &str) -> ModelInfo {
    codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .into_iter()
        .find(|info| info.slug == slug)
        .unwrap()
}

fn candidates() -> Vec<RoutingCandidate> {
    vec![
        RoutingCandidate {
            model: model("gpt-6-luna"),
            role: RoutingModelRole::Economy,
        },
        RoutingCandidate {
            model: model("gpt-6.1-sol"),
            role: RoutingModelRole::Balanced,
        },
        RoutingCandidate {
            model: model("gpt-6-astra"),
            role: RoutingModelRole::Capability,
        },
    ]
}

fn request(candidates: &[RoutingCandidate]) -> RoutingRequest<'_> {
    RoutingRequest {
        task: "Translate this short sentence into English.",
        current_model: "gpt-6.1-sol",
        current_effort: Some(ReasoningEffort::Low),
        preference: 0,
        candidates,
        allowed_models: &[],
        required_context_tokens: 5000,
        requires_images: false,
        max_effort: ReasoningEffort::High,
    }
}

#[test]
fn allowlist_can_exclude_the_current_model_without_erasing_its_verified_metadata() {
    let candidates = candidates();
    let allowed = vec!["gpt-6-luna".to_string()];
    let mut request = request(&candidates);
    request.allowed_models = &allowed;
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        selection(
            "gpt-6-luna",
            ReasoningEffort::Low,
            "Task complexity: simple; model role: economy."
        )
    );
}

fn selection(model: &str, effort: ReasoningEffort, reason: &str) -> Option<RoutingSelection> {
    Some(RoutingSelection {
        model: model.into(),
        effort: Some(effort),
        reason: reason.into(),
    })
}

#[test]
fn local_classification_requires_explicit_evidence_and_prioritizes_risk() {
    for (task, expected) in [
        ("", RoutingTaskComplexity::Unknown),
        ("Help me with this.", RoutingTaskComplexity::Unknown),
        ("Translate this sentence.", RoutingTaskComplexity::Simple),
        ("翻译这句话。", RoutingTaskComplexity::Simple),
        (
            "Rename the variable in this single file.",
            RoutingTaskComplexity::Simple,
        ),
        (
            "Fix a typo in one line of the README.",
            RoutingTaskComplexity::Simple,
        ),
        ("只修正一行拼写。", RoutingTaskComplexity::Simple),
        ("单文件变量重命名。", RoutingTaskComplexity::Simple),
        (
            "Rename a variable across the project.",
            RoutingTaskComplexity::Unknown,
        ),
        (
            "Rename the variable in this single file and implement a feature.",
            RoutingTaskComplexity::Standard,
        ),
        (
            "Fix a typo in one line of production authentication.",
            RoutingTaskComplexity::HighStakes,
        ),
        ("Implement a parser.", RoutingTaskComplexity::Standard),
        ("实现一个解析器。", RoutingTaskComplexity::Standard),
        (
            "Find the root cause of a concurrency bug.",
            RoutingTaskComplexity::Demanding,
        ),
        ("分析跨模块竞态的根因。", RoutingTaskComplexity::Demanding),
        (
            "Translate and fix our production authentication flow.",
            RoutingTaskComplexity::HighStakes,
        ),
        ("简单修复支付权限问题。", RoutingTaskComplexity::HighStakes),
        (
            "Translate the compiler error and implement the fix.",
            RoutingTaskComplexity::Standard,
        ),
    ] {
        assert_eq!(classify_task_locally(task), expected, "{task}");
    }
}

#[test]
fn preference_selects_relative_roles_with_complexity_floors() {
    let candidates = candidates();
    let mut request = request(&candidates);
    for (preference, complexity, expected) in [
        (
            0,
            RoutingTaskComplexity::Simple,
            selection(
                "gpt-6-luna",
                ReasoningEffort::Low,
                "Task complexity: simple; model role: economy.",
            ),
        ),
        (
            50,
            RoutingTaskComplexity::Simple,
            selection(
                "gpt-6.1-sol",
                ReasoningEffort::Low,
                "Task complexity: simple; model role: balanced.",
            ),
        ),
        (
            100,
            RoutingTaskComplexity::Simple,
            selection(
                "gpt-6-astra",
                ReasoningEffort::Low,
                "Task complexity: simple; model role: capability.",
            ),
        ),
        (
            0,
            RoutingTaskComplexity::Standard,
            selection(
                "gpt-6.1-sol",
                ReasoningEffort::Medium,
                "Task complexity: standard; model role: balanced.",
            ),
        ),
        (
            0,
            RoutingTaskComplexity::Demanding,
            selection(
                "gpt-6.1-sol",
                ReasoningEffort::High,
                "Task complexity: demanding; model role: balanced.",
            ),
        ),
        (
            0,
            RoutingTaskComplexity::HighStakes,
            selection(
                "gpt-6-astra",
                ReasoningEffort::High,
                "Task complexity: high_stakes; model role: capability.",
            ),
        ),
    ] {
        request.preference = preference;
        assert_eq!(choose_task_model(&request, complexity), expected);
    }
}

#[test]
fn unknown_or_invalid_task_inputs_preserve_current_selection() {
    let candidates = candidates();
    let mut request = request(&candidates);
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Unknown),
        None
    );
    request.current_model = "gpt-6.1-sol-custom";
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        None
    );
    request.current_model = "gpt-6.1-sol";
    request.preference = 101;
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        None
    );
    request.preference = 0;
    request.required_context_tokens = -1;
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        None
    );
    request.required_context_tokens = 5000;
    request.task = " ";
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        None
    );
}

#[test]
fn a_service_simple_answer_cannot_lower_an_explicit_local_risk_floor() {
    let candidates = candidates();
    let mut request = request(&candidates);
    request.task = "Fix the production authentication flow.";
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Simple),
        selection(
            "gpt-6-astra",
            ReasoningEffort::High,
            "Task complexity: high_stakes; model role: capability."
        )
    );
}

#[test]
fn metadata_eligibility_rejects_fallback_hidden_specialty_and_retired_models() {
    for invalid in 0..7 {
        let mut candidates = candidates();
        let model = &mut candidates[0].model;
        match invalid {
            0 => model.used_fallback_model_metadata = true,
            1 => model.visibility = ModelVisibility::Hide,
            2 => model.model_specialty = Some("cyber".into()),
            3 => model.description = Some("Retired model; do not use.".into()),
            4 => model.description = None,
            5 => model.context_window = None,
            6 => model.supported_reasoning_levels.clear(),
            _ => unreachable!(),
        }
        if invalid == 5 {
            model.max_context_window = None;
        }
        assert_eq!(
            choose_task_model(&request(&candidates), RoutingTaskComplexity::Simple),
            selection(
                "gpt-6.1-sol",
                ReasoningEffort::Low,
                "Task complexity: simple; model role: balanced."
            )
        );
    }
}

#[test]
fn context_and_image_requirements_filter_the_economy_candidate() {
    let mut candidates = candidates();
    candidates[0].model.input_modalities = vec![InputModality::Text];
    let mut routing_request = request(&candidates);
    routing_request.requires_images = true;
    assert_eq!(
        choose_task_model(&routing_request, RoutingTaskComplexity::Simple),
        selection(
            "gpt-6.1-sol",
            ReasoningEffort::Low,
            "Task complexity: simple; model role: balanced."
        )
    );
    candidates[0]
        .model
        .input_modalities
        .push(InputModality::Image);
    candidates[0].model.context_window = Some(5000);
    let routing_request = request(&candidates);
    assert_eq!(
        choose_task_model(&routing_request, RoutingTaskComplexity::Simple),
        selection(
            "gpt-6.1-sol",
            ReasoningEffort::Low,
            "Task complexity: simple; model role: balanced."
        )
    );
}

#[test]
fn effort_must_be_advertised_and_obey_the_cap_without_automatic_ultra() {
    let mut candidates = candidates();
    candidates[0]
        .model
        .supported_reasoning_levels
        .retain(|level| level.effort == ReasoningEffort::Medium);
    assert_eq!(
        choose_task_model(&request(&candidates), RoutingTaskComplexity::Simple),
        selection(
            "gpt-6-luna",
            ReasoningEffort::Medium,
            "Task complexity: simple; model role: economy."
        )
    );
    let mut request = request(&candidates);
    request.max_effort = ReasoningEffort::Low;
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Demanding),
        None
    );
    request.max_effort = ReasoningEffort::Ultra;
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::HighStakes),
        selection(
            "gpt-6-astra",
            ReasoningEffort::High,
            "Task complexity: high_stakes; model role: capability."
        )
    );
    request.max_effort = ReasoningEffort::Custom("future".into());
    assert_eq!(
        choose_task_model(&request, RoutingTaskComplexity::Demanding),
        None
    );
}

#[test]
fn duplicate_catalog_entries_and_missing_capability_preserve_current() {
    let mut candidates = candidates();
    candidates.push(candidates[0].clone());
    assert_eq!(
        choose_task_model(&request(&candidates), RoutingTaskComplexity::Simple),
        None
    );
    candidates.truncate(/*len*/ 2);
    assert_eq!(
        choose_task_model(&request(&candidates), RoutingTaskComplexity::HighStakes),
        None
    );
}

#[test]
fn role_defaults_do_not_infer_from_prefix_or_missing_metadata() {
    let mut info = model("gpt-6-luna");
    info.slug = "gpt-6-luna-future".into();
    assert_eq!(default_routing_role(&info), None);
    info.slug = "custom/gpt-6-luna".into();
    assert_eq!(default_routing_role(&info), None);
    info.slug = "gpt-6-luna".into();
    info.used_fallback_model_metadata = true;
    assert_eq!(default_routing_role(&info), None);
    info.used_fallback_model_metadata = false;
    info.description = Some("Unclassified custom deployment.".into());
    assert_eq!(default_routing_role(&info), None);
}
