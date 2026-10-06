use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> ModelRoutingView {
    let config = ModelRoutingConfigToml::default();
    ModelRoutingView {
        effective_config: config.clone(),
        config,
        overridden: false,
        user_config_version: "captured-version".into(),
        models: vec![],
        last_decision: None,
        decision_service_ready: false,
    }
}

#[test]
fn empty_model_catalog_keeps_a_refresh_action_on_the_model_page() {
    let view = fixture();
    insta::assert_snapshot!(
        "manager_empty_model_catalog",
        format!(
            "{}\n---\n{}",
            render_models(&view, Locale::English),
            render_models(&view, Locale::SimplifiedChinese)
        )
    );
}

#[test]
fn model_selection_page_distinguishes_local_preview_and_external_consent() {
    let mut view = fixture();
    let local = render(&view, Locale::English);
    view.config.mode = ModelRoutingMode::Preview;
    view.config.source = ModelRoutingSource::DecisionService;
    view.config.send_task_description = true;
    view.config.preference = 25;
    view.effective_config = view.config.clone();
    view.decision_service_ready = true;
    insta::assert_snapshot!(format!(
        "{local}\n---\n{}",
        render(&view, Locale::SimplifiedChinese)
    ));
}

#[test]
fn saving_one_setting_preserves_other_settings_and_captured_version() {
    let mut view = fixture();
    view.config.allowed_models = vec!["custom-model".into()];
    view.config.model_roles.insert(
        "custom-model".into(),
        codex_config::ModelRoutingRole::Balanced,
    );
    let mut next = view.config.clone();
    next.preference = 75;
    assert_eq!(
        save_operation(&view, next.clone()).unwrap(),
        AccountManagerOperation::RoutingSave {
            config: next,
            expected_version: Some("captured-version".into()),
        }
    );
}

#[test]
fn invalid_balance_and_unconsented_external_activation_cannot_be_saved() {
    let view = fixture();
    let mut config = view.config.clone();
    config.preference = 101;
    assert!(save_operation(&view, config).is_err());
    let config = ModelRoutingConfigToml {
        mode: ModelRoutingMode::Automatic,
        source: ModelRoutingSource::DecisionService,
        ..ModelRoutingConfigToml::default()
    };
    assert!(save_operation(&view, config).is_err());
}
