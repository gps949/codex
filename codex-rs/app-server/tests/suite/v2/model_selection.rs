use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelSelectionIntent;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::ThreadSettingsUpdatedNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_models_manager::bundled_models_response;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_rpc_releases_manual_pin_and_follow_thread_preserves_it() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("manual"),
            responses::sse_completed("echo"),
            responses::sse_completed("automatic"),
        ],
    )
    .await;
    let home = TempDir::new()?;
    let template = bundled_models_response()?.models.remove(0);
    let models = ["routing-balanced", "routing-economy"]
        .into_iter()
        .map(|slug| {
            let mut model = template.clone();
            model.slug = slug.into();
            model.description = Some("Synthetic routing fixture".into());
            model.guardian = None;
            model.node_repl_auto_review_required = false;
            model.auto_review_model_override = None;
            model.visibility = ModelVisibility::List;
            model.model_specialty = None;
            model.supported_in_api = true;
            model.use_responses_lite = false;
            model.tool_mode = None;
            model.multi_agent_version = None;
            model.model_messages = Some(codex_protocol::openai_models::ModelMessages {
                instructions_template: Some("You are a helpful coding assistant.".into()),
                ..Default::default()
            });
            model.upgrade = None;
            model.comp_hash = None;
            model.context_window = Some(200_000);
            model.default_reasoning_level = Some(ReasoningEffort::Medium);
            model.supported_reasoning_levels = [
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ]
            .into_iter()
            .map(|effort| ReasoningEffortPreset {
                description: effort.to_string(),
                effort,
            })
            .collect();
            model
        })
        .collect::<Vec<_>>();
    let catalog_path = home.path().join("routing-models.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&json!({"models": models}))?,
    )?;
    MockResponsesConfig::new(&server.uri()).with_model("routing-balanced").with_root_config(&format!(
        "model_reasoning_effort = 'medium'\nmodel_catalog_json = {}\n",
        serde_json::to_string(&catalog_path)?,
    )).with_extra_config("[model_routing]\nmode = 'automatic'\npreference = 0\n[model_routing.model_roles]\nrouting-balanced = 'balanced'\nrouting-economy = 'economy'\n").write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let thread = app.start_thread(ThreadStartParams::default()).await?.thread;
    for (index, intent) in [None, Some(ModelSelectionIntent::FollowThread), None]
        .into_iter()
        .enumerate()
    {
        if index == 2 {
            let _: ThreadSettingsUpdateResponse = app
                .request(|request_id| ClientRequest::ThreadSettingsUpdate {
                    request_id,
                    params: ThreadSettingsUpdateParams {
                        thread_id: thread.id.clone(),
                        model_selection_intent: Some(ModelSelectionIntent::Automatic),
                        ..Default::default()
                    },
                })
                .await?;
            loop {
                let updated: ThreadSettingsUpdatedNotification =
                    app.read_notification("thread/settings/updated").await?;
                if updated.thread_settings.model_selection_intent
                    == Some(ModelSelectionIntent::Automatic)
                {
                    break;
                }
            }
        }
        let _: TurnStartResponse = app
            .request(|request_id| ClientRequest::TurnStart {
                request_id,
                params: TurnStartParams {
                    thread_id: thread.id.clone(),
                    model_selection_intent: intent,
                    model: Some("routing-balanced".into()),
                    effort: Some(ReasoningEffort::Medium),
                    collaboration_mode: (index == 2).then(|| CollaborationMode {
                        mode: ModeKind::Default,
                        settings: Settings {
                            model: "routing-balanced".into(),
                            reasoning_effort: Some(ReasoningEffort::Medium),
                            developer_instructions: None,
                        },
                    }),
                    input: vec![UserInput::Text {
                        text: "Translate this sentence".into(),
                        text_elements: Vec::new(),
                    }],
                    ..Default::default()
                },
            })
            .await?;
        let _: TurnCompletedNotification = app.read_notification("turn/completed").await?;
    }
    assert_eq!(
        inference
            .requests()
            .into_iter()
            .map(|request| {
                let body = request.body_json();
                (body["model"].clone(), body["reasoning"]["effort"].clone())
            })
            .collect::<Vec<_>>(),
        vec![
            (json!("routing-balanced"), json!("medium")),
            (json!("routing-balanced"), json!("medium")),
            (json!("routing-economy"), json!("low")),
        ]
    );
    Ok(())
}
