use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn api_inventory_derives_key_availability_from_the_captured_revision() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    let manager = AccountManager::new(config);
    let store = manager.api_store();
    let account = store.add(
        codex_login::ApiAccount {
            id: String::new(),
            label: "API fixture".into(),
            base_url: "https://provider.example/v1".into(),
            model: "fixture-model".into(),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        "synthetic-key",
    )?;
    let usable_revision = store.credential_revision(&account.id)?.unwrap();
    let (views, _) = manager.api_inventory()?;
    assert_eq!(
        serde_json::to_value(views)?,
        serde_json::json!([{
            "id": account.id,
            "label": "API fixture",
            "baseUrl": "https://provider.example/v1",
            "model": "fixture-model",
            "disabled": false,
            "contextWindow": 32_768,
            "images": false,
            "hasKey": true,
            "credentialRevision": usable_revision,
        }])
    );

    std::fs::write(
        store.credential_home(&account.id)?.join("auth.json"),
        br#"{"OPENAI_API_KEY":""}"#,
    )?;
    let (views, _) = manager.api_inventory()?;
    assert_eq!(
        serde_json::to_value(views)?,
        serde_json::json!([{
            "id": account.id,
            "label": "API fixture",
            "baseUrl": "https://provider.example/v1",
            "model": "fixture-model",
            "disabled": false,
            "contextWindow": 32_768,
            "images": false,
            "hasKey": false,
            "credentialRevision": null,
        }])
    );
    Ok(())
}
