use codex_config::DecisionAdvisorConfigToml;
use codex_config::DecisionAdvisorModeToml;
use codex_config::DecisionAdvisorProviderToml;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionAdvisorProvider;
use codex_model_provider::DecisionAdvisorSettings;
use std::io;
use std::time::Duration;

pub(super) fn resolve(
    config: Option<&DecisionAdvisorConfigToml>,
) -> io::Result<DecisionAdvisorSettings> {
    let Some(config) = config else {
        return Ok(DecisionAdvisorSettings::default());
    };
    let provider = match config.provider {
        DecisionAdvisorProviderToml::Typesafe => DecisionAdvisorProvider::Typesafe,
        DecisionAdvisorProviderToml::Cloudflare => DecisionAdvisorProvider::Cloudflare,
    };
    let settings = DecisionAdvisorSettings {
        mode: match config.mode {
            DecisionAdvisorModeToml::Off => DecisionAdvisorMode::Off,
            DecisionAdvisorModeToml::Shadow => DecisionAdvisorMode::Shadow,
            DecisionAdvisorModeToml::Rank => DecisionAdvisorMode::Rank,
        },
        provider,
        suggest_skills: config.suggest_skills,
        endpoint: config.endpoint.clone().unwrap_or_else(|| match provider {
            DecisionAdvisorProvider::Typesafe => "https://api.typesafe.ai/v1/systemone".into(),
            DecisionAdvisorProvider::Cloudflare => String::new(),
        }),
        model: config.model.clone().unwrap_or_else(|| match provider {
            DecisionAdvisorProvider::Typesafe => "jev-1.13.0".into(),
            DecisionAdvisorProvider::Cloudflare => "clef-flash".into(),
        }),
        api_key_env: config
            .api_key_env
            .clone()
            .unwrap_or_else(|| match provider {
                DecisionAdvisorProvider::Typesafe => "TYPESAFE_API_KEY".into(),
                DecisionAdvisorProvider::Cloudflare => "CLOUDFLARE_AI_TOKEN".into(),
            }),
        allow_local_http: config.allow_local_http,
        timeout: Duration::from_millis(config.timeout_ms.unwrap_or(650)),
        min_confidence: config.min_confidence.unwrap_or(0.35),
    };
    settings
        .validate()
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    Ok(settings)
}
