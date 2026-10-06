use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> NativeMenuSession {
    let config = codex_config::ModelRoutingConfigToml::default();
    NativeMenuSession::new(
        FrozenAccountInventory {
            accounts: vec![],
            total: 0,
            paused: false,
            settings: serde_json::json!({}),
            primary: None,
            fallback: codex_login::ApiAccountFallback::default(),
            routing: Some(crate::account_management::ModelRoutingView {
                effective_config: config.clone(),
                config,
                overridden: false,
                user_config_version: "captured-version".into(),
                models: vec![],
                last_decision: None,
                decision_service_ready: true,
            }),
            reset_journals: vec![],
            reset_total: 0,
        },
        NativeMenuEntry::Manage,
    )
}

fn render(question: MenuQuestion) -> String {
    format!(
        "{}\n{}",
        question.text,
        question
            .choices
            .into_iter()
            .map(|choice| format!("{}\n{}", choice.label, choice.description))
            .collect::<Vec<_>>()
            .join("\n\n")
    )
}

#[test]
fn native_model_menu_keeps_headings_short_and_data_in_descriptions() {
    let session = fixture();
    for page in [
        RoutingPage::Mode,
        RoutingPage::Controls,
        RoutingPage::Source,
        RoutingPage::Preference,
        RoutingPage::Advanced,
        RoutingPage::Effort,
        RoutingPage::Models(0),
    ] {
        let question = session
            .inventory
            .routing_question(page, NativeAccountLanguage::English);
        assert!(!question.text.contains('\n'));
        assert!(question.text.chars().count() <= 56);
        assert!((2..=7).contains(&question.choices.len()));
        let labels = question
            .choices
            .iter()
            .map(|choice| &choice.label)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(labels.len(), question.choices.len());
        assert!(
            question
                .choices
                .iter()
                .all(|choice| choice.description.chars().count() <= 640)
        );
    }
    let english = session
        .inventory
        .routing_question(RoutingPage::Mode, NativeAccountLanguage::English);
    let chinese = session
        .inventory
        .routing_question(RoutingPage::Controls, NativeAccountLanguage::Chinese);
    assert!(!english.text.contains('\n'));
    assert!(english.choices[0].description.contains("50/100"));
    insta::assert_snapshot!(
        "mobile_model_selection",
        format!("{}\n---\n{}", render(english), render(chinese))
    );
}

#[test]
fn external_source_requires_visible_confirm_with_captured_version() {
    let mut session = fixture();
    session
        .prepare_routing(
            RoutingChange::Source(ModelRoutingSource::DecisionService),
            NativeAccountLanguage::Chinese,
        )
        .unwrap();
    let expected = codex_config::ModelRoutingConfigToml {
        source: ModelRoutingSource::DecisionService,
        send_task_description: true,
        ..Default::default()
    };
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::RoutingSave {
            config: expected,
            expected_version: Some("captured-version".into()),
        }
    );
    assert_eq!(
        session.inventory.routing.as_ref().unwrap().config.source,
        ModelRoutingSource::Local
    );
    insta::assert_snapshot!(
        "mobile_model_service_consent",
        render(session.question(NativeAccountLanguage::Chinese))
    );
}

#[test]
fn returning_this_thread_to_auto_does_not_grant_global_or_external_consent() {
    let mut session = fixture();
    let before = session.inventory.routing.as_ref().unwrap().config.clone();
    session.prepare_thread_model_automatic(NativeAccountLanguage::English);
    assert_eq!(
        session.pending.as_ref().unwrap().operation,
        AccountManagerOperation::ThreadModelAutomatic
    );
    assert_eq!(session.inventory.routing.as_ref().unwrap().config, before);
    assert_eq!(
        session.pending.as_ref().unwrap().return_page,
        MenuPage::Routing(RoutingPage::Mode)
    );
    insta::assert_snapshot!(
        "mobile_thread_model_automatic_confirmation",
        render(session.question(NativeAccountLanguage::English))
    );
}

#[test]
fn numeric_preference_rejects_invalid_text_and_preserves_other_policy_fields() {
    let mut session = fixture();
    session.page = MenuPage::Routing(RoutingPage::CustomPreference);
    assert!(
        session
            .prepare_routing_text("101", NativeAccountLanguage::English)
            .is_err()
    );
    assert!(
        session
            .prepare_routing_text("2.5", NativeAccountLanguage::English)
            .is_err()
    );
    session
        .inventory
        .routing
        .as_mut()
        .unwrap()
        .config
        .allowed_models = vec!["exact-model".into()];
    assert!(
        session
            .prepare_routing_text("75", NativeAccountLanguage::English)
            .unwrap()
    );
    let AccountManagerOperation::RoutingSave { config, .. } =
        &session.pending.as_ref().unwrap().operation
    else {
        unreachable!()
    };
    assert_eq!(
        (config.preference, config.allowed_models.clone()),
        (75, vec!["exact-model".into()])
    );
}

#[test]
fn stale_native_policy_cannot_overwrite_a_new_version_or_service_readiness() {
    let session = fixture();
    let mut fresh = fixture();
    let config = session.inventory.routing.as_ref().unwrap().config.clone();
    fresh
        .inventory
        .routing
        .as_mut()
        .unwrap()
        .user_config_version = "new-version".into();
    assert!(
        super::super::targets::validate_settings(
            &AccountManagerOperation::RoutingSave {
                config: config.clone(),
                expected_version: Some("captured-version".into()),
            },
            &session.inventory,
            &fresh.inventory
        )
        .is_err()
    );
    let view = fresh.inventory.routing.as_mut().unwrap();
    view.user_config_version = "captured-version".into();
    view.decision_service_ready = false;
    let mut external = config;
    external.source = ModelRoutingSource::DecisionService;
    external.send_task_description = true;
    assert!(
        super::super::targets::validate_settings(
            &AccountManagerOperation::RoutingSave {
                config: external,
                expected_version: Some("captured-version".into()),
            },
            &session.inventory,
            &fresh.inventory
        )
        .is_err()
    );
}
