use super::*;
use codex_config::LoaderOverrides;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn current_host_requirements_reject_remote_or_disallowed_auth() -> anyhow::Result<()> {
    for (requirements, expected_message) in [
        ("allow_remote_control = false", "Remote Control is disabled"),
        (
            "allowed_login_methods = [\"chatgpt\"]",
            "disallowed by current requirements",
        ),
    ] {
        let home = tempdir()?;
        let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("synthetic-key"));
        let mut overrides = LoaderOverrides::without_managed_config_for_tests();
        if requirements.starts_with("allowed_login_methods") {
            let path = home.path().join("requirements.toml");
            std::fs::write(&path, requirements)?;
            overrides.system_requirements_path = Some(path);
        }
        let policy = HostLoginPolicyLoader {
            config: ConfigManager::new_for_tests(
                home.path().to_path_buf(),
                Vec::new(),
                overrides,
                CloudConfigBundleFixture::loader_with_enterprise_requirement(requirements),
            ),
            source: Arc::new(Mutex::new(Arc::downgrade(&manager))),
            chatgpt_base_url: "http://127.0.0.1:9/backend-api".into(),
            http_client_factory: manager.http_client_factory(),
        };
        let error = policy.prepare(manager).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains(expected_message), "{error}");
    }
    Ok(())
}

#[tokio::test]
async fn repeated_host_requests_retain_the_same_cloud_config_loader() -> anyhow::Result<()> {
    let home = tempdir()?;
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("synthetic-key"));
    let policy = HostLoginPolicyLoader {
        config: ConfigManager::new_for_tests(
            home.path().to_path_buf(),
            Vec::new(),
            LoaderOverrides::without_managed_config_for_tests(),
            CloudConfigBundleFixture::loader_with_enterprise_requirement(
                "allow_remote_control = false",
            ),
        ),
        source: Arc::new(Mutex::new(Arc::downgrade(&manager))),
        chatgpt_base_url: "http://127.0.0.1:9/backend-api".into(),
        http_client_factory: manager.http_client_factory(),
    };
    for _ in 0..2 {
        assert_eq!(
            policy
                .prepare(Arc::clone(&manager))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    Ok(())
}

#[tokio::test]
async fn startup_remote_permission_uses_machine_requirements_and_not_root_cloud_denial()
-> anyhow::Result<()> {
    let home = tempdir()?;
    let manager = ConfigManager::new_for_tests(
        home.path().to_path_buf(),
        Vec::new(),
        LoaderOverrides::without_managed_config_for_tests(),
        CloudConfigBundleFixture::loader_with_enterprise_requirement(
            "allow_remote_control = false",
        ),
    );
    assert!(manager.local_remote_control_allowed().await?);
    let path = home.path().join("requirements.toml");
    std::fs::write(&path, "allow_remote_control = false")?;
    let manager = ConfigManager::new_for_tests(
        home.path().to_path_buf(),
        Vec::new(),
        LoaderOverrides {
            system_requirements_path: Some(path),
            ..LoaderOverrides::without_managed_config_for_tests()
        },
        Default::default(),
    );
    assert!(!manager.local_remote_control_allowed().await?);
    Ok(())
}
