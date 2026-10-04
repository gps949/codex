use super::*;
use crate::DecisionAdvisorProvider;
use pretty_assertions::assert_eq;

#[test]
fn saved_decision_credentials_are_bound_to_the_full_service_target() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let store = DecisionAdvisorCredentialStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    );
    let mut settings = DecisionAdvisorSettings {
        mode: DecisionAdvisorMode::Shadow,
        credential_source: DecisionAdvisorCredentialSource::Stored,
        endpoint: "https://service.example/accounts/first/decision".into(),
        ..Default::default()
    };
    store.save(
        &settings,
        &DecisionAdvisorSecret::from("synthetic-decision-key"),
    )?;
    assert_eq!(
        store.load(&settings)?,
        Some(DecisionAdvisorSecret::from("synthetic-decision-key"))
    );
    settings.model = "jev-another-model".into();
    assert_eq!(
        store.load(&settings)?,
        Some(DecisionAdvisorSecret::from("synthetic-decision-key"))
    );
    settings.endpoint = "https://service.example/accounts/second/decision".into();
    assert_eq!(store.load(&settings)?, None);
    settings.endpoint = "https://service.example/accounts/first/decision".into();
    settings.provider = DecisionAdvisorProvider::Cloudflare;
    settings.model = "clef-flash".into();
    assert_eq!(store.load(&settings)?, None);
    assert!(!home.path().join("auth.json").exists());
    assert!(!home.path().join("api-accounts.json").exists());
    assert_eq!(
        format!(
            "{:?}",
            DecisionAdvisorSecret::from("synthetic-decision-key")
        ),
        "<redacted>"
    );
    Ok(())
}

#[test]
fn missing_saved_credentials_do_not_fall_back_to_the_environment() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let store = DecisionAdvisorCredentialStore::new(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    );
    let mut settings = DecisionAdvisorSettings::default();
    assert_eq!(
        store.resolve(&settings, |_| Some("synthetic-environment-key".into()))?,
        Some(DecisionAdvisorSecret::from("synthetic-environment-key"))
    );
    settings.credential_source = DecisionAdvisorCredentialSource::Stored;
    assert_eq!(
        store.resolve(&settings, |_| panic!(
            "stored credentials must not read environment"
        ))?,
        None
    );
    store.save(
        &settings,
        &DecisionAdvisorSecret::from("synthetic-stored-key"),
    )?;
    store.remove(&settings)?;
    assert_eq!(
        store.resolve(&settings, |_| panic!(
            "removed stored credentials must not fall back"
        ))?,
        None
    );
    Ok(())
}
