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

fn confirmation_account() -> ApiAccount {
    ApiAccount {
        id: String::new(),
        label: "Confirmation fixture".into(),
        base_url: "https://provider.example/v1".into(),
        model: "fixture-model".into(),
        disabled: false,
        context_window: 32_768,
        images: false,
    }
}

#[test]
fn api_management_snapshot_binds_metadata_to_the_saved_key_revision() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = ApiAccountStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let account = ApiAccount {
        id: "api-confirmation".into(),
        ..confirmation_account()
    };
    let missing_key = ApiAccount {
        id: "api-no-key".into(),
        ..confirmation_account()
    };
    let state = ApiAccountState {
        accounts: vec![account.clone(), missing_key],
        selection: ApiAccountSelection::Manual {
            profile_id: account.id.clone(),
        },
        fallback: ApiAccountFallback {
            enabled: true,
            profile_id: Some(account.id.clone()),
            wait_minutes: 7,
        },
        revision: 9,
    };
    std::fs::write(
        home.path().join("api-accounts.json"),
        serde_json::to_vec(&state)?,
    )?;
    let credential_home = store.credential_home(&account.id)?;
    std::fs::create_dir_all(&credential_home)?;
    std::fs::write(
        credential_home.join("auth.json"),
        br#"{"OPENAI_API_KEY":"synthetic-key"}"#,
    )?;

    let snapshot = store.management_snapshot()?;
    assert_eq!(
        serde_json::json!({
            "state": snapshot.state,
            "credentialRevisions": snapshot.credential_revisions,
        }),
        serde_json::json!({
            "state": state,
            "credentialRevisions": {
                "api-confirmation": "5a4218c652ef1d18d0ed319c5c2e8a06765c9d85f89a73e7366edf515414905c",
            },
        })
    );
    Ok(())
}

#[test]
fn api_paid_operations_require_a_confirmed_credential_revision() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = ApiAccountStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let account = store.add(confirmation_account(), "synthetic-key")?;
    let before = serde_json::to_value(store.load()?)?;
    assert!(
        store
            .select_checked(
                ApiAccountSelection::Manual {
                    profile_id: account.id.clone(),
                },
                /*expected_revision*/ None,
            )
            .is_err()
    );
    assert!(
        store
            .configure_fallback_checked(
                ApiAccountFallback {
                    enabled: true,
                    profile_id: Some(account.id),
                    wait_minutes: 5,
                },
                /*expected_revision*/ None,
            )
            .is_err()
    );
    assert_eq!(serde_json::to_value(store.load()?)?, before);
    Ok(())
}

#[test]
fn api_paid_operations_reject_key_endpoint_and_model_changes_since_confirmation()
-> anyhow::Result<()> {
    #[derive(Debug)]
    enum ChangedField {
        Key,
        Endpoint,
        Model,
    }
    for changed_field in [
        ChangedField::Key,
        ChangedField::Endpoint,
        ChangedField::Model,
    ] {
        let home = tempfile::TempDir::new()?;
        let store = ApiAccountStore::new(
            home.path().to_path_buf(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        );
        let mut account = store.add(confirmation_account(), "synthetic-first")?;
        let confirmed = store.management_snapshot()?;
        let confirmed_revision = confirmed.credential_revisions[&account.id].clone();
        match changed_field {
            ChangedField::Key => store.replace_key(&account.id, "synthetic-replacement")?,
            ChangedField::Endpoint => {
                account.base_url = "https://replacement.example/v1".into();
                store.update(account.clone())?;
            }
            ChangedField::Model => {
                account.model = "replacement-model".into();
                store.update(account.clone())?;
            }
        }
        let changed = store.management_snapshot()?;
        assert_eq!(&changed.state.accounts, &vec![account.clone()]);
        assert_ne!(
            changed.credential_revisions.get(&account.id),
            Some(&confirmed_revision)
        );
        let before = serde_json::to_value(changed.state)?;
        assert!(
            store
                .select_checked(
                    ApiAccountSelection::Manual {
                        profile_id: account.id.clone(),
                    },
                    Some(&confirmed_revision),
                )
                .is_err(),
            "Selection must reject a changed {changed_field:?}"
        );
        assert!(
            store
                .configure_fallback_checked(
                    ApiAccountFallback {
                        enabled: true,
                        profile_id: Some(account.id),
                        wait_minutes: 5,
                    },
                    Some(&confirmed_revision),
                )
                .is_err(),
            "Paid fallback must reject a changed {changed_field:?}"
        );
        assert_eq!(serde_json::to_value(store.load()?)?, before);
    }
    Ok(())
}

#[test]
fn api_paid_operations_accept_the_current_credential_revision() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = ApiAccountStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let account = store.add(confirmation_account(), "synthetic-key")?;
    let confirmed_revision = store.credential_revision(&account.id)?.unwrap();
    let other = store.add(confirmation_account(), "synthetic-other-key")?;
    store.replace_key(&other.id, "synthetic-other-replacement")?;

    store.select_checked(
        ApiAccountSelection::Manual {
            profile_id: account.id.clone(),
        },
        Some(&confirmed_revision),
    )?;
    let fallback = ApiAccountFallback {
        enabled: true,
        profile_id: Some(account.id.clone()),
        wait_minutes: 5,
    };
    store.configure_fallback_checked(fallback.clone(), Some(&confirmed_revision))?;
    assert_eq!(
        serde_json::to_value(store.load()?)?,
        serde_json::to_value(ApiAccountState {
            accounts: vec![account.clone(), other.clone()],
            selection: ApiAccountSelection::Manual {
                profile_id: account.id.clone(),
            },
            fallback,
            revision: 4,
        })?
    );
    store.select_checked(
        ApiAccountSelection::Subscription,
        /*expected_revision*/ None,
    )?;
    store.configure_fallback_checked(
        ApiAccountFallback::default(),
        /*expected_revision*/ None,
    )?;
    assert_eq!(
        serde_json::to_value(store.load()?)?,
        serde_json::to_value(ApiAccountState {
            accounts: vec![account, other],
            selection: ApiAccountSelection::Subscription,
            fallback: ApiAccountFallback::default(),
            revision: 6,
        })?
    );
    Ok(())
}
