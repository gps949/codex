use super::*;
use crate::AuthKeyringBackendKind;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;

#[test]
fn api_accounts_are_manual_by_default_and_do_not_store_keys_in_metadata() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = ApiAccountStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let account = store.add(
        ApiAccount {
            id: String::new(),
            label: "API fixture".into(),
            base_url: "https://provider.example/v1".into(),
            model: "fixture-model".into(),
            disabled: false,
            context_window: 32768,
            images: false,
        },
        "synthetic-key",
    )?;
    let state = store.load()?;
    assert_eq!(
        (state.selection, state.fallback.enabled),
        (ApiAccountSelection::Subscription, false)
    );
    assert!(
        !std::fs::read_to_string(home.path().join("api-accounts.json"))?.contains("synthetic-key")
    );
    store.select(ApiAccountSelection::Manual {
        profile_id: account.id.clone(),
    })?;
    assert_eq!(
        store.load()?.selection,
        ApiAccountSelection::Manual {
            profile_id: account.id.clone()
        }
    );
    store.remove(&account.id)?;
    assert_eq!(
        (store.load()?.accounts.len(), store.load()?.selection),
        (0, ApiAccountSelection::Subscription)
    );
    Ok(())
}

#[test]
fn api_account_rejects_destination_credentials_and_nonlocal_cleartext() {
    for endpoint in [
        "http://provider.example/v1",
        "https://secret@provider.example/v1",
        "https://provider.example/?key=secret",
    ] {
        let account = ApiAccount {
            id: "api-fixture".into(),
            label: "Fixture".into(),
            base_url: endpoint.into(),
            model: "model".into(),
            disabled: false,
            context_window: 32768,
            images: false,
        };
        assert!(account.validate().is_err());
    }
}

#[test]
fn api_key_replacement_preserves_target_and_is_available_to_the_next_capture() -> anyhow::Result<()>
{
    let home = tempfile::TempDir::new()?;
    let mode = AuthCredentialsStoreMode::File;
    let backend = AuthKeyringBackendKind::default();
    let store = ApiAccountStore::new(home.path().to_path_buf(), mode, backend);
    let account = store.add(
        ApiAccount {
            id: String::new(),
            label: "API fixture".into(),
            base_url: "https://provider.example/v1".into(),
            model: "fixture-model".into(),
            disabled: false,
            context_window: 32768,
            images: false,
        },
        "synthetic-first",
    )?;
    store.select(ApiAccountSelection::Manual {
        profile_id: account.id.clone(),
    })?;
    let before = serde_json::to_value(store.load()?)?;
    store.replace_key(&account.id, "synthetic-replacement")?;
    assert_eq!(serde_json::to_value(store.load()?)?, before);
    let saved =
        crate::load_auth_dot_json(&store.credential_home(&account.id)?, mode, backend)?.unwrap();
    assert_eq!(
        saved.openai_api_key.as_deref(),
        Some("synthetic-replacement")
    );
    assert!(
        !std::fs::read_to_string(home.path().join("api-accounts.json"))?
            .contains("synthetic-replacement")
    );
    Ok(())
}
