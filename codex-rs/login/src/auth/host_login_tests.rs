use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn config(home: &Path) -> AuthConfig {
    AuthConfig {
        codex_home: home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_login_method: None,
        chatgpt_base_url: None,
        forced_chatgpt_workspace_id: None,
        managed_auth_policy: ManagedAuthPolicy::default(),
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    }
}

fn credentials(user: &str, workspace: &str) -> AuthDotJson {
    use base64::Engine as _;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({
            "email": format!("{user}@example.com"), "https://api.openai.com/auth": {
                "chatgpt_user_id": user, "chatgpt_account_id": workspace
            }
        })
        .to_string(),
    );
    serde_json::from_value(serde_json::json!({
        "auth_mode": "chatgpt", "tokens": {
            "id_token": format!("e30.{claims}.sig"), "access_token": format!("access-{user}"),
            "refresh_token": format!("refresh-{user}"), "account_id": workspace
        }, "last_refresh": "2099-01-01T00:00:00Z"
    }))
    .unwrap()
}

#[tokio::test]
async fn managed_profile_auth_and_quota_checks_ignore_ephemeral_overlays() {
    let home = TempDir::new().unwrap();
    let saved = credentials("stored-seat", "workspace");
    save_auth(
        home.path(),
        &saved,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let overlay = credentials("process-seat", "another-workspace");
    crate::auth::login_with_chatgpt_auth_tokens(
        home.path(),
        &overlay.tokens.unwrap().id_token.raw_jwt,
        "another-workspace",
        Some("pro"),
    )
    .unwrap();
    let manager = AuthManager::shared_managed_profile_from_auth_config(config(home.path())).await;
    let auth = manager.auth().await.unwrap();
    assert_eq!(auth.get_token_data().unwrap(), saved.tokens.unwrap());
    assert!(
        manager
            .stored_quota_probe_auth_matches(home.path(), &auth)
            .unwrap()
    );
    let replacement = credentials("replacement-seat", "workspace");
    save_auth(
        home.path(),
        &replacement,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    manager.reload().await;
    assert_eq!(
        manager.auth().await.unwrap().get_token_data().unwrap(),
        replacement.tokens.unwrap()
    );
    assert!(
        !manager
            .stored_quota_probe_auth_matches(home.path(), &auth)
            .unwrap()
    );
}

#[tokio::test]
async fn managed_profile_auth_requires_persistent_oauth_and_respects_workspace_policy() {
    let home = TempDir::new().unwrap();
    let saved = credentials("stored-seat", "workspace");
    save_auth(
        home.path(),
        &saved,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let mut restricted = config(home.path());
    restricted.forced_chatgpt_workspace_id = Some(vec!["different-workspace".into()]);
    assert_eq!(restricted.load_managed_profile_auth().await.unwrap(), None);
    let mut ephemeral = config(home.path());
    ephemeral.auth_credentials_store_mode = AuthCredentialsStoreMode::Ephemeral;
    save_auth(
        home.path(),
        &saved,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    assert_eq!(ephemeral.load_managed_profile_auth().await.unwrap(), None);
    let mut pat = saved;
    pat.auth_mode = Some(AuthMode::PersonalAccessToken);
    pat.personal_access_token = Some("at-synthetic-not-a-subscription".into());
    save_auth(
        home.path(),
        &pat,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    assert_eq!(
        config(home.path())
            .load_managed_profile_auth()
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn managed_profiles_ignore_host_workload_markers_while_host_auth_stays_fail_closed()
-> anyhow::Result<()> {
    const CHILD_MARKER: &str = "CODEX_MANAGED_WORKLOAD_AUTH_TEST_CHILD";
    const TEST_NAME: &str = "auth::manager::host_login::tests::managed_profiles_ignore_host_workload_markers_while_host_auth_stays_fail_closed";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_MARKER, "1")
            .env(
                "OPENAI_IDENTITY_TOKEN_FILE",
                "/synthetic-unavailable-identity-assertion",
            )
            .env_remove("OPENAI_FEDERATION_RULE_ID")
            .env_remove("OPENAI_WORKLOAD_IDENTITY_CONTEXT")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("CODEX_API_KEY")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Child workload-isolation test failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let home = TempDir::new()?;
    let saved = credentials("stored-seat", "workspace");
    save_auth(
        home.path(),
        &saved,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    assert!(
        AuthManager::shared_from_auth_config(
            config(home.path()),
            /*enable_codex_api_key_env*/ false
        )
        .await
        .is_err()
    );
    let manager = AuthManager::shared_managed_profile_from_auth_config(config(home.path())).await;
    assert_eq!(
        manager.auth().await.unwrap().get_token_data()?,
        saved.tokens.unwrap()
    );
    assert!(!manager.is_workload_identity_selected());
    manager.reload().await;
    assert_eq!(
        manager
            .auth_cached()
            .unwrap()
            .get_chatgpt_user_id()
            .as_deref(),
        Some("stored-seat")
    );
    Ok(())
}
