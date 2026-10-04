use crate::DecisionAdvisorCredentialSourceToml;
use crate::DecisionAdvisorModeToml;
use crate::DecisionAdvisorProviderToml;
use crate::config_toml::ConfigToml;
use pretty_assertions::assert_eq;

#[test]
fn decision_advisor_options_round_trip_without_literal_credentials() {
    let config: ConfigToml = toml::from_str(
        r#"[decision_advisor]
mode = "shadow"
provider = "cloudflare"
credential_source = "stored"
endpoint = "https://api.cloudflare.com/client/v4/accounts/example/ai/run/@cf/cloudflare/clef-flash"
model = "clef-flash"
api_key_env = "CODEX_DECISION_API_KEY"
timeout_ms = 700
min_confidence = 0.4
suggest_skills = true
"#,
    )
    .unwrap();
    let settings = config.decision_advisor.as_ref().unwrap();
    assert_eq!(
        (settings.mode, settings.provider, settings.credential_source),
        (
            DecisionAdvisorModeToml::Shadow,
            DecisionAdvisorProviderToml::Cloudflare,
            DecisionAdvisorCredentialSourceToml::Stored,
        )
    );
    assert!(settings.suggest_skills);
    let restored: ConfigToml = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(restored.decision_advisor, config.decision_advisor);
}

#[test]
fn unknown_decision_advisor_mode_is_a_config_error() {
    assert!(toml::from_str::<ConfigToml>("[decision_advisor]\nmode = 'automatic'").is_err());
}
