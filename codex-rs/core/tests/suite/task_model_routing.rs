//! Task routing is verified against the actual inference request and retained settings.

use anyhow::Result;
use codex_config::ModelRoutingMode;
use codex_config::ModelRoutingRole;
use codex_core::RecoverTurnRequest;
use codex_core::SteerSubmission;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_models_manager::bundled_models_response;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelSelectionIntent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_protocol::user_input::UserInput;
use codex_utils_image::data_url_from_bytes;
use core_test_support::responses;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use test_case::test_case;

fn catalog() -> ModelsResponse {
    let template = bundled_models_response()
        .expect("bundled model metadata")
        .models
        .remove(0);
    ModelsResponse {
        models: ["routing-balanced", "routing-economy"]
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
                model.model_messages = None;
                model.upgrade = None;
                model.comp_hash = None;
                model.context_window = Some(200_000);
                model.input_modalities = vec![InputModality::Text, InputModality::Image];
                if slug == "routing-balanced" {
                    model.input_modalities.push(InputModality::Audio);
                }
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
            .collect(),
    }
}

fn input(intent: ModelSelectionIntent) -> TurnInputRequest {
    TurnInputRequest::user_input(vec![UserInput::Text {
        text: "Translate this short sentence to English".into(),
        text_elements: Vec::new(),
    }])
    .with_thread_settings(ThreadSettingsOverrides {
        model_selection_intent: Some(intent),
        collaboration_mode: Some(CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: "routing-balanced".into(),
                reasoning_effort: Some(ReasoningEffort::Medium),
                developer_instructions: None,
            },
        }),
        ..Default::default()
    })
}

fn builder(mode: ModelRoutingMode) -> TestCodexBuilder {
    test_codex()
        .with_model("routing-balanced")
        .with_config(move |config| {
            config.model_catalog = Some(catalog());
            config.model_reasoning_effort = Some(ReasoningEffort::Medium);
            config.model_selection_is_explicit = false;
            config.model_routing.mode = mode;
            config.model_routing.preference = 0;
            config
                .model_routing
                .model_roles
                .insert("routing-balanced".into(), ModelRoutingRole::Balanced);
            config
                .model_routing
                .model_roles
                .insert("routing-economy".into(), ModelRoutingRole::Economy);
        })
}

const RETAINED_AUDIO_URL: &str = "data:audio/mpeg;base64,YXVkaW8=";

async fn resume_with_audio_output(
    builder: &mut TestCodexBuilder,
    server: &wiremock::MockServer,
    output_type: &str,
) -> Result<TestCodex> {
    let call = match output_type {
        "function_call_output" => Some(json!({
            "type": "function_call", "name": "recording", "arguments": "{}",
            "call_id": "retained-audio"
        })),
        "custom_tool_call_output" => Some(json!({
            "type": "custom_tool_call", "name": "recording", "input": "record",
            "call_id": "retained-audio"
        })),
        "retained_message_audio" => None,
        _ => panic!("unsupported audio fixture"),
    };
    let mut values = vec![json!({"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "Read this recording"}
    ]})];
    if let Some(call) = call {
        values.push(call);
        values.push(
            json!({"type": output_type, "call_id": "retained-audio", "output": [
                {"type": "input_audio", "audio_url": RETAINED_AUDIO_URL}
            ]}),
        );
    } else {
        values[0]["content"]
            .as_array_mut()
            .expect("message content")
            .push(json!({"type": "input_audio", "audio_url": RETAINED_AUDIO_URL}));
    }
    let items: Vec<ResponseItem> = serde_json::from_value(json!(values))?;
    let rollout = items
        .into_iter()
        .map(|item| RolloutItem::ResponseItem(item.into()))
        .collect::<Vec<_>>();
    let test = builder.build_with_auto_env(server).await?;
    // Persist actual settings before resuming. A transcript without any admitted turn
    // restores the upstream default model, which is not the audio-capable fixture.
    let seed = responses::mount_sse_once(server, responses::sse_completed("audio-settings")).await;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::Explicit))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        seed.single_request().body_json()["model"],
        json!("routing-balanced")
    );
    test.codex.ensure_rollout_materialized().await;
    test.codex.append_rollout_items(&rollout).await?;
    test.codex.shutdown_and_wait().await?;
    // Builder mutators are FnOnce and were consumed by the first build. Restore the
    // synthetic catalog and routing/child features instead of resuming with stock defaults.
    let initial = test.config.clone();
    *builder = std::mem::replace(builder, test_codex()).with_config(move |config| {
        config.model = initial.model;
        config.model_catalog = initial.model_catalog;
        config.model_reasoning_effort = initial.model_reasoning_effort;
        config.model_routing = initial.model_routing;
        config.model_selection_is_explicit = initial.model_selection_is_explicit;
        config.model_selection_override = initial.model_selection_override;
        config.features = initial.features;
    });
    builder
        .resume_with_auto_env(
            server,
            Arc::clone(&test.home),
            test.codex.rollout_path().expect("saved audio history"),
        )
        .await
}

#[test_case("function_call_output"; "function_audio")]
#[test_case("custom_tool_call_output"; "custom_audio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_routing_preserves_retained_tool_audio(output_type: &str) -> Result<()> {
    let server = responses::start_mock_server().await;
    let test = resume_with_audio_output(
        &mut builder(ModelRoutingMode::Automatic),
        &server,
        output_type,
    )
    .await?;
    let inference = responses::mount_sse_once(&server, responses::sse_completed("audio")).await;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::Automatic))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = inference.single_request();
    assert_eq!(
        (
            request.body_json()["model"].clone(),
            request.call_output("retained-audio", output_type)["output"].clone()
        ),
        (
            json!("routing-balanced"),
            json!([{"type": "input_audio", "audio_url": RETAINED_AUDIO_URL}])
        )
    );
    Ok(())
}

#[test_case("inline"; "inline_image")]
#[test_case("local"; "local_image")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_images_route_by_visible_content_instead_of_encoded_size(kind: &str) -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_once(&server, responses::sse_completed("image")).await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    let image_dir = tempfile::tempdir()?;
    let path = image_dir.path().join("routing.png");
    let mut state = 123_456_789u32;
    let pixels = (0..640 * 480 * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    image::RgbImage::from_raw(/*width*/ 640, /*height*/ 480, pixels)
        .expect("RGB image")
        .save(&path)?;
    let bytes = std::fs::read(&path)?;
    assert!(
        bytes.len() > 200_000,
        "fixture exceeds the encoded-byte routing budget"
    );
    let image = match kind {
        "inline" => UserInput::Image {
            image: ImageReference::Inline {
                image_url: data_url_from_bytes("image/png", &bytes),
            },
            detail: None,
        },
        "local" => UserInput::LocalImage { path, detail: None },
        _ => panic!("unsupported image fixture"),
    };
    let mut request = input(ModelSelectionIntent::FollowThread);
    let codex_protocol::turn_input::TurnInput::UserInput { content, .. } = &mut request.input
    else {
        panic!("user input fixture");
    };
    content.push(image);
    test.codex.start_or_steer_turn(request).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = inference.single_request();
    assert_eq!(request.body_json()["model"], json!("routing-economy"));
    assert!(request.input().iter().any(|item| {
        item["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|part| part["type"] == "input_image"))
    }));
    Ok(())
}

#[test_case(ModelRoutingMode::Off, "routing-balanced", ReasoningEffort::Medium; "off")]
#[test_case(ModelRoutingMode::Preview, "routing-balanced", ReasoningEffort::Medium; "preview")]
#[test_case(ModelRoutingMode::Automatic, "routing-economy", ReasoningEffort::Low; "automatic")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_root_task_routes_actual_request(
    mode: ModelRoutingMode,
    expected_model: &str,
    expected_effort: ReasoningEffort,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference =
        responses::mount_sse_once(&server, responses::sse_completed("routing-response")).await;
    let test = builder(mode).build_with_auto_env(&server).await?;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let body = inference.single_request().body_json();
    assert_eq!(
        (body["model"].clone(), body["reasoning"]["effort"].clone()),
        (json!(expected_model), json!(expected_effort))
    );
    let observation_path = test
        .config
        .codex_home
        .join("model-routing-observation.json");
    if mode == ModelRoutingMode::Off {
        assert!(!observation_path.exists());
    } else {
        let bytes = std::fs::read(observation_path)?;
        assert!(bytes.len() <= 4096);
        let observation: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            (
                observation["model"].clone(),
                observation["effort"].clone(),
                observation["scope"].clone(),
                observation["applied"].clone()
            ),
            (
                json!("routing-economy"),
                json!("low"),
                json!("main"),
                json!(mode == ModelRoutingMode::Automatic)
            )
        );
        assert!(observation.get("task").is_none() && observation.get("history").is_none());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_value_manual_pin_survives_follow_thread_until_automatic() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("explicit"),
            responses::sse_completed("follow"),
            responses::sse_completed("automatic"),
        ],
    )
    .await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    for intent in [
        ModelSelectionIntent::Explicit,
        ModelSelectionIntent::FollowThread,
        ModelSelectionIntent::Automatic,
    ] {
        test.codex.start_or_steer_turn(input(intent)).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        let expected = if intent == ModelSelectionIntent::Automatic {
            intent
        } else {
            ModelSelectionIntent::Explicit
        };
        assert_eq!(
            test.codex
                .thread_settings_snapshot()
                .await
                .model_selection_intent,
            Some(expected)
        );
    }
    let selected = inference
        .requests()
        .into_iter()
        .map(|request| {
            let body = request.body_json();
            (body["model"].clone(), body["reasoning"]["effort"].clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        vec![
            (json!("routing-balanced"), json!("medium")),
            (json!("routing-balanced"), json!("medium")),
            (json!("routing-economy"), json!("low")),
        ]
    );
    Ok(())
}

#[test_case(Some("priority"), "routing-balanced"; "turn_tier_is_preserved")]
#[test_case(None, "routing-economy"; "default_tier_can_use_economy")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_model_routing_preserves_the_actual_turn_service_tier(
    tier: Option<&str>,
    model: &str,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_once(&server, responses::sse_completed("tier")).await;
    let test = builder(ModelRoutingMode::Automatic)
        .with_config(|config| {
            config
                .features
                .enable(Feature::FastMode)
                .expect("enable fast mode for tier fixture");
            let mut models = catalog();
            for model in &mut models.models {
                model.service_tiers.clear();
                if model.slug == "routing-balanced" {
                    model
                        .service_tiers
                        .push(codex_protocol::openai_models::ModelServiceTier {
                            id: "priority".into(),
                            name: "Fast".into(),
                            description: "Fixture tier".into(),
                        });
                }
            }
            config.model_catalog = Some(models);
        })
        .build_with_auto_env(&server)
        .await?;
    let mut request = input(ModelSelectionIntent::FollowThread);
    request.start.service_tier = tier.map(str::to_owned);
    test.codex.start_or_steer_turn(request).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let body = inference.single_request().body_json();
    assert_eq!(
        (body["model"].clone(), body["service_tier"].clone()),
        (json!(model), json!(tier))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_resume_model_override_wins_over_the_owned_automatic_policy() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("auto"),
            responses::sse_completed("override"),
        ],
    )
    .await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.shutdown_and_wait().await?;
    let resumed = builder(ModelRoutingMode::Automatic)
        .with_config(|config| {
            config.model_selection_override = Some(ModelSelectionIntent::Explicit);
            config.model = Some("routing-balanced".into());
        })
        .resume_with_auto_env(
            &server,
            Arc::clone(&test.home),
            test.codex.rollout_path().expect("saved rollout"),
        )
        .await?;
    assert_eq!(
        resumed
            .codex
            .thread_settings_snapshot()
            .await
            .model_selection_intent,
        Some(ModelSelectionIntent::Explicit)
    );
    resumed
        .codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        inference
            .requests()
            .into_iter()
            .map(|request| request.body_json()["model"].clone())
            .collect::<Vec<_>>(),
        vec![json!("routing-economy"), json!("routing-balanced")]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn released_pin_survives_legacy_turn_echo_and_distinct_selection_pins() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("manual"),
            responses::sse_completed("legacy-automatic"),
            responses::sse_completed("distinct-manual"),
            responses::sse_completed("retained-manual"),
        ],
    )
    .await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::Explicit))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model_selection_intent: Some(ModelSelectionIntent::Automatic),
            ..Default::default()
        },
    )
    .await?;
    let mut echo = input(ModelSelectionIntent::FollowThread);
    echo.thread_settings.model_selection_intent = None;
    test.codex.start_or_steer_turn(echo).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        test.codex
            .thread_settings_snapshot()
            .await
            .model_selection_intent,
        Some(ModelSelectionIntent::Automatic)
    );
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some("routing-economy".into()),
            effort: Some(Some(ReasoningEffort::Low)),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(
        test.codex
            .thread_settings_snapshot()
            .await
            .model_selection_intent,
        Some(ModelSelectionIntent::Explicit)
    );
    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model_selection_intent: Some(ModelSelectionIntent::Automatic),
            ..Default::default()
        },
    )
    .await?;
    for _ in 0..2 {
        let mut manual = input(ModelSelectionIntent::Explicit);
        manual.thread_settings = ThreadSettingsOverrides {
            model: Some("routing-balanced".into()),
            effort: Some(Some(ReasoningEffort::High)),
            ..Default::default()
        };
        test.codex.start_or_steer_turn(manual).await?;
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        assert_eq!(
            test.codex
                .thread_settings_snapshot()
                .await
                .model_selection_intent,
            Some(ModelSelectionIntent::Explicit)
        );
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
            (json!("routing-economy"), json!("low")),
            (json!("routing-balanced"), json!("high")),
            (json!("routing-balanced"), json!("high")),
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_pin_is_restored_from_owned_rollout_settings() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("pin"),
            responses::sse_completed("resumed"),
        ],
    )
    .await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::Explicit))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex.shutdown_and_wait().await?;
    let resumed = builder(ModelRoutingMode::Automatic)
        .resume_with_auto_env(
            &server,
            Arc::clone(&test.home),
            test.codex.rollout_path().expect("saved rollout"),
        )
        .await?;
    assert_eq!(
        resumed
            .codex
            .thread_settings_snapshot()
            .await
            .model_selection_intent,
        Some(ModelSelectionIntent::Explicit)
    );
    resumed
        .codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        inference
            .requests()
            .into_iter()
            .map(|request| request.body_json()["model"].clone())
            .collect::<Vec<_>>(),
        vec![json!("routing-balanced"), json!("routing-balanced")]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_retains_the_admitted_model_and_effort() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse_completed("task"),
            responses::sse_completed("retry"),
        ],
    )
    .await;
    let test = builder(ModelRoutingMode::Automatic)
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    test.codex
        .recover_turn_if_idle(RecoverTurnRequest {
            turn_id: "recovered-admitted-choice".into(),
            thread_settings: Default::default(),
            trace: None,
            cyber_access_program: None,
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
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
            (json!("routing-economy"), json!("low")),
            (json!("routing-economy"), json!("low")),
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steering_a_simple_request_preserves_the_active_choice() -> Result<()> {
    let server = responses::start_mock_server().await;
    let inference = responses::mount_sse_sequence(&server, vec![responses::sse(vec![
        responses::ev_response_created("paused"),
        responses::ev_function_call("pause-routing", "request_user_input", &json!({"questions": [{
            "id": "continue", "header": "Continue", "question": "Continue the current task?",
            "options": [{"label": "Yes (Recommended)", "description": "Continue the task."}, {"label": "No", "description": "Stop the task."}],
        }]}).to_string()),
        responses::ev_completed("paused"),
    ]), responses::sse_completed("continued")]).await;
    let test = builder(ModelRoutingMode::Automatic)
        .with_config(|config| {
            config
                .features
                .enable(Feature::DefaultModeRequestUserInput)
                .expect("allow request_user_input");
            config.permissions.approval_policy =
                codex_core::config::Constrained::allow_any(AskForApproval::OnRequest);
        })
        .build_with_auto_env(&server)
        .await?;
    let mut request = input(ModelSelectionIntent::FollowThread);
    if let codex_core::TurnInput::UserInput { content, .. } = &mut request.input {
        content[0] = UserInput::Text {
            text: "Explain this part while waiting for clarification".into(),
            text_elements: Vec::new(),
        };
    }
    let TurnInputSubmission::Started { turn_id } = test.codex.start_or_steer_turn(request).await?
    else {
        panic!("new task expected");
    };
    let question = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::RequestUserInput(question) => Some(question.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        test.codex
            .steer_turn(input(ModelSelectionIntent::FollowThread), turn_id.clone())
            .await?,
        SteerSubmission::Steered { turn_id }
    );
    test.codex
        .submit(Op::UserInputAnswer {
            id: question.turn_id,
            response: RequestUserInputResponse {
                answers: HashMap::from([(
                    "continue".into(),
                    RequestUserInputAnswer {
                        answers: vec!["Yes (Recommended)".into()],
                    },
                )]),
            },
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        inference
            .requests()
            .into_iter()
            .map(|request| request.body_json()["model"].clone())
            .collect::<Vec<_>>(),
        vec![json!("routing-balanced"), json!("routing-balanced")]
    );
    Ok(())
}

#[test_case("none", None, "routing-economy", ReasoningEffort::Low; "fresh_child")]
#[test_case("1", None, "routing-economy", ReasoningEffort::Low; "partial_child")]
#[test_case("all", None, "routing-balanced", ReasoningEffort::Medium; "full_fork")]
#[test_case("2", Some("function_call_output"), "routing-economy", ReasoningEffort::Low; "partial_function_audio_filtered")]
#[test_case("2", Some("custom_tool_call_output"), "routing-economy", ReasoningEffort::Low; "partial_custom_audio_filtered")]
#[test_case("2", Some("retained_message_audio"), "routing-balanced", ReasoningEffort::Medium; "partial_retained_message_audio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_creation_routes_only_fresh_or_partial_history(
    fork_turns: &str,
    audio_output_type: Option<&str>,
    expected_model: &str,
    expected_effort: ReasoningEffort,
) -> Result<()> {
    let server = responses::start_mock_server().await;
    let mut builder = builder(ModelRoutingMode::Automatic).with_config(|config| {
        config.model_routing.main_tasks = false;
        config
            .features
            .enable(Feature::Collab)
            .expect("allow collaboration");
        config
            .features
            .enable(Feature::MultiAgentV2)
            .expect("allow multi-agent v2");
        config
            .features
            .disable(Feature::EnableRequestCompression)
            .expect("plain fixture requests");
    });
    let test = if let Some(output_type) = audio_output_type {
        resume_with_audio_output(&mut builder, &server, output_type).await?
    } else {
        builder.build_with_auto_env(&server).await?
    };
    let parent_id = test.session_configured.thread_id.to_string();
    let initial_id = parent_id.clone();
    responses::mount_sse_once_match(&server, move |request: &wiremock::Request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("plain JSON request");
        body["client_metadata"]["thread_id"] == initial_id
            && !body["input"].as_array().expect("request input").iter().any(|item| item["call_id"] == "route-child-spawn")
    }, responses::sse(vec![
        responses::ev_response_created("parent-spawn"),
        responses::ev_function_call_with_namespace("route-child-spawn", "collaboration", "spawn_agent", &json!({
            "task_name": "translation_worker", "message": "Translate this short sentence", "fork_turns": fork_turns,
        }).to_string()),
        responses::ev_completed("parent-spawn"),
    ])).await;
    let child_parent_id = parent_id.clone();
    let child_request = responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("plain JSON request");
            body["client_metadata"]["thread_id"] != child_parent_id
        },
        responses::sse_completed("child-done"),
    )
    .await;
    responses::mount_sse_once_match(
        &server,
        move |request: &wiremock::Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("plain JSON request");
            body["client_metadata"]["thread_id"] == parent_id
                && body["input"]
                    .as_array()
                    .expect("request input")
                    .iter()
                    .any(|item| item["call_id"] == "route-child-spawn")
        },
        responses::sse_completed("parent-done"),
    )
    .await;
    let mut created = test.thread_manager.subscribe_thread_created();
    test.codex
        .start_or_steer_turn(input(ModelSelectionIntent::FollowThread))
        .await?;
    let child_id = created.recv().await?;
    let child = test.thread_manager.get_thread(child_id).await?;
    wait_for_event(&child, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    // ResponseMock records before the custom matcher, including nonmatching parent requests.
    // Select the worker's real POSTs by its runtime thread metadata.
    let child_posts = child_request
        .requests()
        .into_iter()
        .map(|request| request.body_json())
        .filter(|body| body["client_metadata"]["thread_id"] == json!(child_id))
        .collect::<Vec<_>>();
    assert_eq!(
        child_posts
            .iter()
            .map(|body| (body["model"].clone(), body["reasoning"]["effort"].clone()))
            .collect::<Vec<_>>(),
        vec![(json!(expected_model), json!(expected_effort))]
    );
    if audio_output_type == Some("retained_message_audio") {
        let audio = child_posts[0]["input"]
            .as_array()
            .expect("child input")
            .iter()
            .flat_map(|item| item["content"].as_array().into_iter().flatten())
            .find(|part| part["type"] == "input_audio")
            .expect("retained message audio");
        assert_eq!(
            audio,
            &json!({"type": "input_audio", "audio_url": RETAINED_AUDIO_URL})
        );
    }
    child.shutdown_and_wait().await?;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
