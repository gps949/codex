use super::*;
use crate::AccountProfileMetadataUpdate;
use crate::AuthKeyringBackendKind;
use crate::primary_login::tests::config;
use crate::primary_login::tests::credentials;
use crate::primary_login::tests::ready_profile;
use crate::primary_login::tests::write_auth;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn root_observation_is_local_and_does_not_treat_api_tokens_as_subscription_identity() {
    let home = TempDir::new().unwrap();
    let config = config(home.path().to_path_buf());
    let missing = observe_primary_login(&config);
    assert_eq!(
        (missing.status, missing.source, missing.email),
        (PrimaryLoginStatus::NeedsLogin, "root".into(), None),
    );
    write_auth(home.path(), &credentials("root-user", "workspace", "root"));
    let root = observe_primary_login(&config);
    assert_eq!(
        root,
        PrimaryLoginObservation {
            source: "root".into(),
            revision: 0,
            profile_id: None,
            label: "Root login".into(),
            email: Some("root-user@example.com".into()),
            message: None,
            status: PrimaryLoginStatus::StoredReady,
        },
    );
    let api = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("synthetic-api-key".into()),
        ..credentials("obsolete-oauth", "workspace", "old")
    };
    write_auth(home.path(), &api);
    let blocked = observe_primary_login(&config);
    assert_eq!(
        (blocked.status, blocked.email),
        (PrimaryLoginStatus::NeedsLogin, None)
    );
    assert!(
        !PrimaryLoginStore::new(home.path().to_path_buf())
            .path()
            .exists()
    );
}

#[test]
fn external_root_overlay_is_observed_without_contacting_network_or_reading_stale_tokens() {
    let home = TempDir::new().unwrap();
    let config = config(home.path().to_path_buf());
    write_auth(
        home.path(),
        &credentials("stored-root", "workspace", "root"),
    );
    let mut external = credentials("process-host", "other-workspace", "external");
    external.auth_mode = Some(AuthMode::ChatgptAuthTokens);
    external.tokens.as_mut().unwrap().refresh_token.clear();
    crate::save_auth(
        home.path(),
        &external,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    assert_eq!(
        observe_primary_login(&config).email.as_deref(),
        Some("process-host@example.com")
    );
    external.auth_mode = Some(AuthMode::PersonalAccessToken);
    external.personal_access_token = Some("at-synthetic-no-network".into());
    crate::save_auth(
        home.path(),
        &external,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let pat = observe_primary_login(&config);
    assert_eq!(
        (pat.status, pat.email),
        (PrimaryLoginStatus::RuntimeResolutionRequired, None)
    );
    external.auth_mode = Some(AuthMode::AgentIdentity);
    external.personal_access_token = None;
    crate::save_auth(
        home.path(),
        &external,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let agent = observe_primary_login(&config);
    assert_eq!(
        (agent.status, agent.email),
        (PrimaryLoginStatus::RuntimeResolutionRequired, None)
    );
}

#[tokio::test]
async fn selected_profile_ignores_overlays_binds_owner_and_checks_policy() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "seat", "workspace");
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    profiles
        .update_profile_metadata(
            &profile.id,
            AccountProfileMetadataUpdate {
                disabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let config = config(home.path().to_path_buf());
    let selected = PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&config, &profile.id)
        .await
        .unwrap();
    crate::save_auth(
        &profile.credential_home,
        &credentials("overlay", "workspace", "overlay"),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    assert_eq!(
        observe_primary_login(&config),
        PrimaryLoginObservation {
            source: "profile".into(),
            revision: selected.revision,
            profile_id: Some(profile.id.to_string()),
            label: "seat@example.com".into(),
            email: Some("seat@example.com".into()),
            message: None,
            status: PrimaryLoginStatus::StoredReady,
        }
    );
    let mut restricted = config.clone();
    restricted.forced_chatgpt_workspace_id = Some(vec!["other-workspace".into()]);
    assert_eq!(
        observe_primary_login(&restricted).status,
        PrimaryLoginStatus::NeedsLogin
    );
    restricted.forced_chatgpt_workspace_id = None;
    restricted.forced_login_method = Some(ForcedLoginMethod::Api);
    assert_eq!(
        observe_primary_login(&restricted).status,
        PrimaryLoginStatus::NeedsLogin
    );
    write_auth(
        &profile.credential_home,
        &credentials("replacement", "workspace", "replacement"),
    );
    let changed = observe_primary_login(&config);
    assert_eq!(
        (changed.status, changed.email),
        (PrimaryLoginStatus::NeedsLogin, None)
    );
    assert!(
        changed
            .message
            .unwrap()
            .contains("Select this host account again")
    );
}

#[test]
fn signed_out_and_invalid_sources_never_display_retained_root_identity() {
    let home = TempDir::new().unwrap();
    let config = config(home.path().to_path_buf());
    write_auth(home.path(), &credentials("root", "workspace", "root"));
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let state = store.sign_out().unwrap();
    assert_eq!(
        observe_primary_login(&config),
        PrimaryLoginObservation {
            source: "signedOut".into(),
            revision: state.revision,
            profile_id: None,
            label: "Signed out".into(),
            email: None,
            message: None,
            status: PrimaryLoginStatus::SignedOut,
        }
    );
    std::fs::write(store.path(), "corrupt metadata").unwrap();
    let invalid = observe_primary_login(&config);
    assert_eq!(
        (invalid.status, invalid.source, invalid.email),
        (PrimaryLoginStatus::Invalid, "invalid".into(), None)
    );
}

#[tokio::test]
async fn external_process_auth_is_runtime_only_while_managed_profiles_remain_local()
-> anyhow::Result<()> {
    const CHILD: &str = "CODEX_PRIMARY_OBSERVATION_TEST_CHILD";
    const NAME: &str = "primary_login_status::tests::external_process_auth_is_runtime_only_while_managed_profiles_remain_local";
    let Some(mode) = std::env::var_os(CHILD) else {
        for mode in ["access", "workload"] {
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .args(["--exact", NAME, "--nocapture"])
                .env(CHILD, mode)
                .env_remove("CODEX_ACCESS_TOKEN")
                .env_remove("CODEX_API_KEY")
                .env_remove("OPENAI_IDENTITY_TOKEN_FILE")
                .env_remove("OPENAI_FEDERATION_RULE_ID")
                .env_remove("OPENAI_WORKLOAD_IDENTITY_CONTEXT");
            if mode == "access" {
                command.env("CODEX_ACCESS_TOKEN", "synthetic-invalid-external-token");
            } else {
                command.env(
                    "OPENAI_IDENTITY_TOKEN_FILE",
                    "/synthetic-unavailable-assertion",
                );
            }
            let output = command.output()?;
            anyhow::ensure!(
                output.status.success(),
                "Child source observation failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return Ok(());
    };
    let home = TempDir::new()?;
    let config = config(home.path().to_path_buf());
    write_auth(home.path(), &credentials("root", "workspace", "root"));
    let root = observe_primary_login(&config);
    assert_eq!(
        (root.status, root.email),
        (PrimaryLoginStatus::RuntimeResolutionRequired, None)
    );
    if mode == "access" {
        let profile = ready_profile(home.path(), "seat", "workspace");
        PrimaryLoginStore::new(home.path().to_path_buf())
            .select_profile(&config, &profile.id)
            .await?;
        let profile = observe_primary_login(&config);
        assert_eq!(
            (profile.status, profile.email),
            (
                PrimaryLoginStatus::StoredReady,
                Some("seat@example.com".into())
            )
        );
    }
    Ok(())
}
