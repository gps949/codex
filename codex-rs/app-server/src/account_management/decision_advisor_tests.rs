use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

#[tokio::test]
async fn enabling_a_new_service_requires_its_own_token_without_saving() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let result = manager.execute(serde_json::from_value(json!({
        "type":"decisionSave", "config":{"mode":"rank","provider":"cloudflare","model":"clef-flash","endpoint":"https://api.cloudflare.com/client/v4/accounts/synthetic/ai/run/@cf/cloudflare/clef-flash","credential_source":"stored"},
        "credential":{"type":"keep"},"consent":true
    }))?).await;
    assert!(
        result
            .err()
            .expect("missing token is rejected")
            .to_string()
            .contains("Enter a token")
    );
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[tokio::test]
async fn decision_save_is_local_and_does_not_expose_the_service_key() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let server = wiremock::MockServer::start().await;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let operation: AccountManagerOperation = serde_json::from_value(json!({
        "type":"decisionSave", "config":{"mode":"off","endpoint":server.uri(),"allow_local_http":true},
        "credential":{"type":"replace","value":"synthetic-decision-key"},"consent":false
    }))?;
    assert!(!format!("{operation:?}").contains("synthetic-decision-key"));
    let result = manager.execute(operation).await?;
    let view = manager.decision_advisor_view().await?;
    assert!(view.credential_present);
    assert_eq!(view.credential_source, "saved");
    assert!(!serde_json::to_string(&result)?.contains("synthetic-decision-key"));
    assert!(!serde_json::to_string(&view)?.contains("synthetic-decision-key"));
    assert!(
        !std::fs::read_to_string(home.path().join("config.toml"))?
            .contains("synthetic-decision-key")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!home.path().join("auth.json").exists());
    Ok(())
}

#[tokio::test]
async fn decision_probe_requires_consent_uses_synthetic_data_and_does_not_save()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "model":"jev-1.13.0", "answers":{"tool":{"type":"choice","choice":"clock","confidence":0.99,"probabilities":{"clock":0.99,"weather":0.0,"none":0.01}}}
    }))).expect(1).mount(&server).await;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let mut payload = json!({"type":"decisionProbe","config":{"mode":"off","endpoint":server.uri(),"allow_local_http":true,"api_key_env":""},"credential":{"type":"keep"},"consent":false});
    assert!(
        manager
            .execute(serde_json::from_value(payload.clone())?)
            .await
            .is_err()
    );
    payload["consent"] = json!(true);
    let result = manager.execute(serde_json::from_value(payload)?).await?;
    assert_eq!(result.data["connected"], json!(true));
    let request = &server.received_requests().await.unwrap()[0];
    let body: serde_json::Value = serde_json::from_slice(&request.body)?;
    assert_eq!(
        body["state"]["query"],
        json!("Find the tool that reports the current time.")
    );
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[tokio::test]
async fn standalone_decision_probe_obeys_loaded_managed_destination_policy() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let requirements = home.path().join("requirements-fixture.toml");
    std::fs::write(
        &requirements,
        "[application.network]\nenabled = true\n[application.network.domains]\n'allowed.example' = 'allow'\n",
    )?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .loader_overrides(codex_config::LoaderOverrides {
            system_requirements_path: Some(requirements.clone()),
            ..Default::default()
        })
        .build()
        .await?;
    std::fs::write(&requirements, "[application.network]\nenabled = false\n")?;
    let manager = AccountManager::new(config);
    let server = wiremock::MockServer::start().await;
    let result = manager.execute(serde_json::from_value(json!({"type":"decisionProbe","config":{"endpoint":server.uri(),"allow_local_http":true,"api_key_env":""},"credential":{"type":"keep"},"consent":true}))?).await?;
    assert_eq!(result.data["connected"], json!(false));
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn decision_save_rejects_stale_versions_before_changing_credentials() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let version = manager.decision_advisor_view().await?.user_config_version;
    std::fs::write(home.path().join("config.toml"), "model = 'user-update'\n")?;
    let result = manager.execute(serde_json::from_value(json!({
        "type":"decisionSave","config":{},"credential":{"type":"replace","value":"synthetic-stale-key"},"consent":false,"expectedVersion":version
    }))?).await;
    assert!(result.is_err());
    assert!(!home.path().join("decision-auth-profiles").exists());
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml"))?,
        "model = 'user-update'\n"
    );
    Ok(())
}

#[tokio::test]
async fn decision_probe_never_uses_a_previous_success_as_connection_evidence() -> anyhow::Result<()>
{
    let home = tempfile::tempdir()?;
    let server = wiremock::MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "model":"jev-1.13.0", "answers":{"tool":{"type":"choice","choice":"clock","confidence":0.99,"probabilities":{"clock":0.99,"weather":0.0,"none":0.01}}}
    }))).expect(2).mount(&server).await;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    let payload = json!({"type":"decisionProbe","config":{"endpoint":server.uri(),"allow_local_http":true},"credential":{"type":"replace","value":"synthetic-draft-key"},"consent":true});
    for _ in 0..2 {
        let result = manager
            .execute(serde_json::from_value(payload.clone())?)
            .await?;
        assert_eq!(result.data, json!({"connected":true,"scope":"synthetic"}));
    }
    assert!(!home.path().join("decision-auth-profiles").exists());
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[tokio::test]
async fn decision_probe_fails_closed_if_local_managed_policy_becomes_unreadable()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let requirements = home.path().join("requirements.toml");
    std::fs::write(&requirements, "[application.network]\nenabled = false\n")?;
    let manager = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(codex_config::LoaderOverrides {
                system_requirements_path: Some(requirements.clone()),
                ..Default::default()
            })
            .build()
            .await?,
    );
    std::fs::write(requirements, "[application.network]\nenabled = [")?;
    let server = wiremock::MockServer::start().await;
    let result = manager.execute(serde_json::from_value(json!({"type":"decisionProbe","config":{"endpoint":server.uri(),"allow_local_http":true,"api_key_env":""},"credential":{"type":"keep"},"consent":true}))?).await?;
    assert_eq!(result.data["connected"], json!(false));
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn decision_probe_preserves_the_live_managed_factory_and_redacts_remote_errors()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let server = wiremock::MockServer::start().await;
    let controller = codex_http_client::NetworkPolicyController::default();
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.application_network_policy = controller.policy();
    let manager = AccountManager::new(config);
    let payload = json!({"type":"decisionProbe","config":{"endpoint":server.uri(),"allow_local_http":true,"api_key_env":""},"credential":{"type":"keep"},"consent":true});
    let denied = manager
        .execute(serde_json::from_value(payload.clone())?)
        .await?;
    assert_eq!(denied.data, json!({"connected":false,"scope":"synthetic"}));
    assert!(server.received_requests().await.unwrap().is_empty());
    controller.publish(
        controller.policy().revision(),
        codex_http_client::DestinationPolicy::Unrestricted,
    );
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("synthetic-private-remote-body"))
        .expect(1)
        .mount(&server)
        .await;
    let error = manager.execute(serde_json::from_value(payload)?).await?;
    assert_eq!(error.data, json!({"connected":false,"scope":"synthetic"}));
    assert!(!serde_json::to_string(&error)?.contains("synthetic-private-remote-body"));
    Ok(())
}
