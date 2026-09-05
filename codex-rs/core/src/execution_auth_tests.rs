use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::config::ConfigBuilder;

#[test]
fn only_managed_chatgpt_on_the_builtin_openai_provider_is_pool_eligible() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let openai = providers["openai"].clone();
    let ollama = providers["ollama"].clone();
    let bedrock = providers["amazon-bedrock"].clone();
    let mut custom_bearer = openai.clone();
    custom_bearer.name = "custom".to_string();
    custom_bearer.requires_openai_auth = false;
    custom_bearer.experimental_bearer_token = Some("custom-token".into());

    let cases = [
        (
            "managed ChatGPT",
            "openai",
            &openai,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Eligible,
        ),
        (
            "managed-only pool before profile resolution",
            "openai",
            &openai,
            None,
            false,
            PoolEligibility::Eligible,
        ),
        (
            "API key",
            "openai",
            &openai,
            Some(AuthMode::ApiKey),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "external ChatGPT tokens",
            "openai",
            &openai,
            Some(AuthMode::ChatgptAuthTokens),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "local provider",
            "ollama",
            &ollama,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "Bedrock",
            "amazon-bedrock",
            &bedrock,
            Some(AuthMode::BedrockApiKey),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "custom bearer",
            "custom",
            &custom_bearer,
            Some(AuthMode::Chatgpt),
            false,
            PoolEligibility::Ineligible,
        ),
        (
            "workload identity",
            "openai",
            &openai,
            Some(AuthMode::Chatgpt),
            true,
            PoolEligibility::Ineligible,
        ),
    ];

    for (name, provider_id, provider, auth_mode, workload_identity_selected, expected) in cases {
        assert_eq!(
            pool_eligibility(provider_id, provider, auth_mode, workload_identity_selected,),
            expected,
            "unexpected eligibility for {name}",
        );
    }
}

#[tokio::test]
async fn non_openai_provider_ignores_configured_account_pool() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("account-profiles.json"),
        "invalid manifest",
    )?;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    config.model_provider_id = "ollama".to_string();
    config.model_provider = built_in_model_providers(/*openai_base_url*/ None)["ollama"].clone();
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("test"),
    ));

    let enabled = execution_auth.ensure_runtime_from_config(&config).await?;

    assert!(!enabled);
    assert!(execution_auth.runtime().is_none());
    Ok(())
}

#[tokio::test]
async fn startup_prewarm_is_skipped_only_for_an_eligible_configured_pool() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await?;
    let execution_auth = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));

    assert!(!execution_auth.should_skip_startup_prewarm(&config, &config.model_provider));

    std::fs::write(
        codex_home.path().join("account-profiles.json"),
        "configured",
    )?;
    assert!(execution_auth.should_skip_startup_prewarm(&config, &config.model_provider));

    let ollama = &built_in_model_providers(/*openai_base_url*/ None)["ollama"];
    assert!(!execution_auth.should_skip_startup_prewarm(&config, ollama));
    Ok(())
}
