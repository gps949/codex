use super::*;
use crate::account_transition::AccountHistoryTransition;
use crate::config::ConfigBuilder;
use base64::Engine as _;
use codex_history::CodexHarnessMetadata;
use codex_login::AccountProfileStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn metadata_bearing_tool_output_keeps_sampling_account_and_existing_metadata()
-> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let mut ids = Vec::new();
    for index in 0..2 {
        let owner = format!("seat-{index}");
        let profile = profiles.allocate_profile(Some(owner.clone()), index)?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({
            "https://api.openai.com/auth": {"chatgpt_user_id": owner, "chatgpt_account_id": owner},
        }).to_string());
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "tokens": {"id_token": format!("e30.{payload}.sig"), "access_token": owner,
                    "refresh_token": "synthetic-refresh", "account_id": owner},
                "last_refresh": "2099-01-01T00:00:00Z",
            }))?,
        )?;
        profiles.complete_profile(&profile.id)?;
        ids.push(profile.id);
    }
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.account_pool.window_warmup = Some(false);
    let execution = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(execution.ensure_runtime_from_config(&config).await?);
    let account_a = execution.active_lease().expect("account A");
    let pool = execution.account_pool().expect("pool");
    pool.activate(&ids[1])?;
    let account_b = execution.active_lease().expect("account B");
    assert_ne!(account_a.profile_id(), account_b.profile_id());

    let (session, turn) = crate::session::tests::make_session_and_context().await;
    set_sampling_execution_provenance(&turn, account_b.clone());
    let step = StepContext::for_test(Arc::new(turn));
    let session = Arc::new(session);
    let item = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("owned-tool-output".into()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputText {
                text: "readable result".into(),
            },
            FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: "account-b-secret".into(),
            },
        ]),
        internal_chat_message_metadata_passthrough: None,
    };
    let envelope = ResponseItemEnvelope {
        item,
        metadata: Some(CodexHarnessMetadata {
            history_truncation_token_limit: Some(2_000),
            retain_across_windows: Some(true),
            ..Default::default()
        }),
    };
    let mut outputs = futures::stream::iter([Ok(envelope)]);
    drain_in_flight(&mut outputs, Arc::clone(&session), &step).await;
    let history = session.clone_history().await;
    let recorded = history.annotated_items().iter().find(|envelope| {
        matches!(&envelope.item, ResponseItem::FunctionCallOutput { call_id: Some(id), .. } if id == "owned-tool-output")
    }).expect("persisted tool output");
    let metadata = recorded.metadata.as_ref().expect("metadata");
    assert_eq!(
        metadata.execution_profile_id.as_deref(),
        Some(ids[1].as_str())
    );
    assert_eq!(metadata.execution_generation, Some(account_b.generation()));
    assert_eq!(metadata.history_truncation_token_limit, Some(2_000));
    assert_eq!(metadata.retain_across_windows, Some(true));
    let root = Some(ids[0].as_str().to_string());
    let (on_a, _) = AccountHistoryTransition::pooled(&account_a, root.clone())
        .prepare_for_request(vec![recorded.clone()])?;
    let (on_b, _) = AccountHistoryTransition::pooled(&account_b, root)
        .prepare_for_request(vec![recorded.clone()])?;
    assert!(!serde_json::to_string(&on_a)?.contains("account-b-secret"));
    assert!(serde_json::to_string(&on_a)?.contains("readable result"));
    assert!(serde_json::to_string(&on_b)?.contains("account-b-secret"));
    Ok(())
}
