use super::*;
use pretty_assertions::assert_eq;

#[test]
fn model_routing_keeps_network_consent_independent_from_automatic_selection() {
    let mut config: ModelRoutingConfigToml =
        toml::from_str("mode='automatic'\npreference=0").unwrap();
    assert_eq!(
        (config.source, config.preference),
        (ModelRoutingSource::Local, 0)
    );
    assert!(config.validate().is_ok());
    config.source = ModelRoutingSource::DecisionService;
    assert!(config.validate().is_err());
    config.send_task_description = true;
    assert!(config.validate().is_ok());
}

#[test]
fn invalid_preferences_and_implicit_ultra_are_rejected() {
    for source in [
        "preference=101",
        "max_effort='ultra'",
        "allowed_models=['']",
        "unknown_option=true",
    ] {
        let parsed = toml::from_str::<ModelRoutingConfigToml>(source);
        assert!(
            parsed.is_err() || parsed.unwrap().validate().is_err(),
            "{source}"
        );
    }
}
