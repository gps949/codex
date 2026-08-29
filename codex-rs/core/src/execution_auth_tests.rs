use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::built_in_model_providers;
use tempfile::TempDir;

use super::*;
use crate::config::ConfigBuilder;

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
