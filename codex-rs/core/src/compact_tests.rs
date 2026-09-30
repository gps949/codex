use super::*;
use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use codex_code_mode::CellId;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::ExecutedToolCall;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageReference;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_tools::ToolName;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;

#[test_case(true; "metadata enabled")]
#[test_case(false; "metadata disabled after capture")]
#[tokio::test]
async fn local_compaction_respects_tool_metadata_state(
    metadata_enabled: bool,
) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let provider = ModelProviderInfo::create_openai_provider(Some(format!("{}/v1", server.uri())));
    let (session, turn, _events) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        Vec::new(),
        move |config| {
            config.model = Some("gpt-5.2".to_string());
            config.model_provider = provider;
            config.model_provider.supports_websockets = false;
            config
                .features
                .enable(Feature::ExecutedToolCallMetadata)
                .expect("enable tool-call metadata");
        },
    )
    .await;

    let mut items = vec![user_message("Update the plan")];
    for index in 0..300 {
        let call_id = format!("direct-{index}");
        let arguments = json!({"plan": [{"step": "x".repeat(7 * 1024), "status": "completed"}]});
        assert!(serde_json::to_vec(&arguments)?.len() < 8 * 1024);
        items.push(ResponseItem::FunctionCall {
            id: None,
            name: "update_plan".to_string(),
            namespace: None,
            arguments: arguments.to_string(),
            encrypted_function_args: None,
            call_id: call_id.clone(),
            internal_chat_message_metadata_passthrough: None,
        });
        let mut output = ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some(call_id),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload::from_text("Plan updated".to_string()),
            internal_chat_message_metadata_passthrough: Some(
                InternalChatMessageMetadataPassthrough {
                    turn_id: Some("metadata-canary".to_string()),
                    create_time: Some(123.into()),
                    ..Default::default()
                },
            ),
        };
        output.append_executed_tool_calls(vec![ExecutedToolCall::new(
            "update_plan".to_string(),
            arguments,
        )]);
        output.mark_tool_calls_complete();
        items.push(output);
    }
    let cell = CellId::new("local-compaction-cell".to_string());
    let nested_call = ExecutedToolCall::new("nested_tool".to_string(), json!({}));
    let recorder = &session.services.executed_tool_calls;
    recorder.start_cell(&cell, "exec");
    recorder.record_tool_call(
        &ToolCall {
            tool_name: ToolName::plain("nested_tool"),
            call_id: "nested".to_string(),
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
            encrypted_function_args: None,
        },
        &ToolCallSource::CodeMode {
            cell_id: cell.as_str().to_string(),
            runtime_tool_call_id: "nested".to_string(),
        },
        &StepContext::for_test(Arc::clone(&turn)),
    );
    recorder.finish_cell_recording(&cell);
    items.push(serde_json::from_value(json!({
        "type": "custom_tool_call", "call_id": "exec", "name": "exec", "input": "",
    }))?);
    items.push(serde_json::from_value(json!({
        "type": "custom_tool_call_output", "call_id": "exec", "output": "done",
    }))?);
    session
        .record_conversation_items(&turn, turn.model_info(), &items)
        .await;
    let live_history = session.clone_history().await;
    let mut expected_code_mode_output = live_history
        .raw_items()
        .find(|item| matches!(item, ResponseItem::CustomToolCallOutput { .. }))
        .expect("Code Mode output recorded")
        .clone();
    if metadata_enabled {
        expected_code_mode_output.append_executed_tool_calls(vec![nested_call]);
        expected_code_mode_output.set_tool_call_cell_id("exec");
        expected_code_mode_output.mark_tool_calls_complete();
    }
    let outputs = live_history
        .raw_items()
        .filter_map(|item| match item {
            ResponseItem::FunctionCallOutput { .. } => Some(serde_json::to_value(item)),
            _ => None,
        })
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(outputs.len(), 300);
    let metadata_bytes: usize = outputs
        .iter()
        .map(|item| {
            serde_json::to_vec(&item["internal_chat_message_metadata_passthrough"])
                .unwrap()
                .len()
        })
        .sum();
    // Compaction does not rebudget source records as a normal inference request.
    // Passthrough bytes are also excluded from model token estimates.
    assert!(metadata_bytes > 2 * 1024 * 1024);

    if !metadata_enabled {
        let mut config = (*session.get_config().await).clone();
        config.features.disable(Feature::ExecutedToolCallMetadata)?;
        session.refresh_runtime_config(config).await;
    }

    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("summary", "The prior calls finished."),
            responses::ev_completed("compact-response"),
        ]),
    )
    .await;
    // OpenAI identity keeps the client from removing passthrough for compatibility.
    run_compact_task(
        Arc::clone(&session),
        turn,
        vec![UserInput::Text {
            text: "Summarize the conversation.".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await?;

    let request = mock.single_request();
    assert!(request.inputs_of_type("compaction_trigger").is_empty());
    assert_eq!(
        request.custom_tool_call_output("exec"),
        serde_json::to_value(expected_code_mode_output)?,
    );
    for mut output in outputs {
        let call_id = output["call_id"].as_str().expect("source call id");
        let compact_output = request.function_call_output(call_id);
        assert_eq!(compact_output["output"], json!("Plan updated"));
        if !metadata_enabled {
            let metadata = output["internal_chat_message_metadata_passthrough"]
                .as_object_mut()
                .expect("source metadata");
            metadata.remove("executed_tool_calls");
            metadata.remove("tool_calls_complete");
        }
        assert_eq!(
            compact_output["internal_chat_message_metadata_passthrough"],
            output["internal_chat_message_metadata_passthrough"]
        );
    }
    let compacted_history = session.clone_history().await;
    let expected_summary = format!("{SUMMARY_PREFIX}\nThe prior calls finished.");
    assert!(compacted_history.raw_items().any(|item| {
        matches!(item, ResponseItem::Message { role, content, .. }
            if role == "user"
                && content_items_to_text(content).as_deref() == Some(expected_summary.as_str()))
    }));
    Ok(())
}

fn annotated(items: Vec<ResponseItem>) -> Vec<ResponseItemEnvelope> {
    items.into_iter().map(ResponseItemEnvelope::new).collect()
}

fn raw(items: Vec<ResponseItemEnvelope>) -> Vec<ResponseItem> {
    items
        .into_iter()
        .map(ResponseItemEnvelope::into_item)
        .collect()
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn compacted_user_message<'a>(text: &str, original: &'a ResponseItem) -> CompactedUserMessage<'a> {
    CompactedUserMessage {
        message: text.to_string(),
        original,
        harness_metadata: None,
    }
}

#[test]
fn local_compaction_output_buffer_has_hard_item_and_token_limits() {
    let mut item_limited = LocalCompactionOutputBuffer::default();
    for index in 0..MAX_LOCAL_COMPACTION_OUTPUT_ITEMS {
        item_limited
            .push(user_message(&format!("item-{index}")))
            .expect("items up to the hard count limit should fit");
    }
    let count_error = item_limited
        .push(user_message("one-too-many"))
        .expect_err("the 65th item must exceed the hard count limit");
    assert!(matches!(
        count_error.details(),
        CodexErrorDetails::Stream(message) if message.contains("output limit")
    ));

    let mut token_limited = LocalCompactionOutputBuffer::default();
    let oversized = user_message(&"x".repeat(
        usize::try_from(MAX_LOCAL_COMPACTION_OUTPUT_TOKENS).unwrap_or_default() * 4 + 1_024,
    ));
    let token_error = token_limited
        .push(oversized)
        .expect_err("oversized output must exceed the token limit");
    assert!(matches!(
        token_error.details(),
        CodexErrorDetails::Stream(message) if message.contains("output limit")
    ));
    assert!(token_limited.items().is_empty());
}

#[test]
fn content_items_to_text_joins_non_empty_segments() {
    let items = vec![
        ContentItem::InputText {
            text: "hello".to_string(),
        },
        ContentItem::OutputText {
            text: String::new(),
        },
        ContentItem::OutputText {
            text: "world".to_string(),
        },
    ];

    let joined = content_items_to_text(&items);

    assert_eq!(Some("hello\nworld".to_string()), joined);
}

#[test]
fn content_items_to_text_ignores_image_only_content() {
    let items = vec![ContentItem::InputImage {
        image: ImageReference::Inline {
            image_url: "file://image.png".to_string(),
        },
        detail: Some(DEFAULT_IMAGE_DETAIL),
    }];

    let joined = content_items_to_text(&items);

    assert_eq!(None, joined);
}

#[test]
fn collect_user_messages_extracts_user_text_only() {
    let items = vec![
        ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", "assistant")),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: "ignored".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", "user")),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "first".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Other,
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(vec![compacted_user_message("first", &items[1])], collected,);
}

#[test_case(json!([{"type": "input_text", "text": "first"}]), "first", true; "unchanged text")]
#[test_case(json!([
    {"type": "input_text", "text": "first"},
    {"type": "input_text", "text": ""},
    {"type": "input_text", "text": "second"}
]), "firstsecond", true; "text boundaries")]
#[test_case(json!([
    {"type": "input_text", "text": "first"},
    {"type": "input_image", "image_url": "file://image.png"}
]), "first", false; "omitted media")]
fn collect_annotated_user_messages_extracts_user_text_only(
    input_content: serde_json::Value,
    expected_text: &str,
    preserve_content: bool,
) {
    let source = json!({
        "id": {"message_id": "source", "turn_id": "turn", "role": "user"},
        "revision": "retained_revision", "complete": true,
    });
    let metadata: CodexHarnessMetadata = serde_json::from_value(json!({
        "retained_source": source, "guardian_sources": [source],
        "guardian_source_order_guidance": true, "user_input_order": 7,
    }))
    .unwrap();
    let mut item = user_message("first");
    if let ResponseItem::Message { content, .. } = &mut item {
        *content = serde_json::from_value(input_content).unwrap();
    }
    let items = vec![
        ResponseItemEnvelope {
            item: item.clone(),
            metadata: Some(metadata.clone()),
        },
        ResponseItemEnvelope::new(ResponseItem::Other),
    ];

    let collected = collect_annotated_user_messages(&items);

    if !preserve_content {
        item = user_message(expected_text);
    }
    let expected = ResponseItemEnvelope {
        item,
        metadata: Some(metadata),
    };
    assert_eq!(
        collected,
        vec![CompactedUserMessage {
            message: expected_text.to_owned(),
            original: &items[0].item,
            harness_metadata: items[0].metadata.as_ref(),
        }]
    );
    assert_eq!(
        build_compacted_history(Vec::new(), &collected, "summary"),
        vec![
            expected,
            ResponseItemEnvelope::new(ContextualUserFragment::into(CompactionSummary::new(
                "summary"
            ))),
        ],
    );
}

#[test]
fn collect_user_messages_filters_session_prefix_entries() {
    let items = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: r#"# AGENTS.md instructions for project

<INSTRUCTIONS>
do things
</INSTRUCTIONS>"#
                    .to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "<ENVIRONMENT_CONTEXT>cwd=/tmp</ENVIRONMENT_CONTEXT>".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "real user message".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(
        vec![compacted_user_message("real user message", &items[2])],
        collected
    );
}

#[test]
fn collect_user_messages_filters_legacy_warnings() {
    let items = vec![
        user_message(
            "Warning: The maximum number of unified exec processes you can keep open is 60 and you currently have 61 processes open. Reuse older processes or close them to prevent automatic pruning of old processes",
        ),
        user_message(
            "Warning: apply_patch was requested via exec_command. Use the apply_patch tool instead of exec_command.",
        ),
        user_message(
            "Warning: Your account was flagged for potentially high-risk cyber activity and this request was routed to gpt-5.2 as a fallback. To regain access to gpt-5.3-codex, apply for trusted access: https://chatgpt.com/cyber or learn more: https://developers.openai.com/codex/concepts/cyber-safety",
        ),
        user_message("real user message"),
    ];

    let collected = collect_user_messages(&items);

    assert_eq!(
        vec![compacted_user_message("real user message", &items[3])],
        collected
    );
}

#[test]
fn build_token_limited_compacted_history_truncates_overlong_user_messages() {
    // Use a small truncation limit so the test remains fast while still validating
    // that oversized user content is truncated.
    let max_tokens = 16;
    let big = "word ".repeat(200);
    let mut original = ResponseItemEnvelope::new(user_message(&big));
    original
        .item
        .set_id(Some(ResponseItemId::with_suffix("msg", "long-user")));
    original.metadata = Some(CodexHarnessMetadata::default());
    let history = super::build_compacted_history_with_limit(
        Vec::new(),
        &collect_annotated_user_messages(std::slice::from_ref(&original)),
        "SUMMARY",
        max_tokens,
    );
    assert_eq!(history.len(), 2);

    let truncated_message = &history[0].item;
    let summary_message = &history[1].item;

    let truncated_text = match truncated_message {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("unexpected item in history: {other:?}"),
    };

    assert!(
        truncated_text.contains("tokens truncated"),
        "expected truncation marker in truncated user message"
    );
    assert!(
        !truncated_text.contains(&big),
        "truncated user message should not include the full oversized user text"
    );

    let summary_text = match summary_message {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("unexpected item in history: {other:?}"),
    };
    assert_eq!(summary_text, "SUMMARY");
    assert_eq!(history[0].id(), original.id());
    assert_eq!(history[0].metadata, Some(CodexHarnessMetadata::default()));
    assert_eq!(history[1].metadata, None);
}

#[test]
fn portable_compaction_stops_when_aggregate_budget_cannot_retain_an_older_message() {
    let mut items = (0..256)
        .map(|index| user_message(&format!("old-{index}:{}", "x".repeat(128))))
        .collect::<Vec<_>>();
    items.push(user_message("tail"));
    let user_messages = collect_user_messages(&items);

    let history = super::build_compacted_history_with_limit(
        Vec::new(),
        &user_messages,
        "SUMMARY",
        /*max_tokens*/ 2,
    );
    let retained_user_texts = history[..history.len() - 1]
        .iter()
        .map(|envelope| match &envelope.item {
            ResponseItem::Message { content, .. } => {
                content_items_to_text(content).expect("retained user message should contain text")
            }
            other => panic!("expected retained user message, found {other:?}"),
        })
        .collect::<Vec<_>>();

    assert_eq!(retained_user_texts, vec!["tail".to_string()]);
}

#[test]
fn build_token_limited_compacted_history_appends_summary_message() {
    let initial_context: Vec<ResponseItemEnvelope> = Vec::new();
    let original = user_message("first user message");
    let user_messages = collect_user_messages(std::slice::from_ref(&original));
    let summary_text = "summary text";

    let history = build_compacted_history(initial_context, &user_messages, summary_text);
    assert!(
        !history.is_empty(),
        "expected compacted history to include summary"
    );

    let last = history.last().expect("history should have a summary entry");
    let summary = match &last.item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            content_items_to_text(content).unwrap_or_default()
        }
        other => panic!("expected summary message, found {other:?}"),
    };
    assert_eq!(summary, summary_text);
}

#[test]
fn portable_compaction_caps_each_user_and_summary_item() {
    let items = [
        user_message(&format!("older:{}:older-tail", "a".repeat(32_000))),
        user_message(&format!("middle:{}:middle-tail", "b".repeat(32_000))),
        user_message(&format!("recent:{}:recent-tail", "c".repeat(32_000))),
    ];
    let summary = format!("summary:{}:summary-tail", "d".repeat(32_000));

    let history = build_compacted_history(Vec::new(), &collect_user_messages(&items), &summary);

    assert_eq!(history.len(), 4);
    let estimates = history
        .iter()
        .map(|envelope| crate::context_manager::estimate_item_token_count(&envelope.item))
        .collect::<Vec<_>>();
    assert!(
        estimates
            .iter()
            .all(|tokens| *tokens <= MAX_PORTABLE_CONTEXT_ITEM_TOKENS as i64),
        "portable item estimates exceeded the cap: {estimates:?}",
    );
    let retained_user_bytes = history[..history.len() - 1]
        .iter()
        .map(|envelope| match &envelope.item {
            ResponseItem::Message { content, .. } => content_items_to_text(content)
                .expect("retained user message should contain text")
                .len(),
            other => panic!("expected retained user message, found {other:?}"),
        })
        .sum::<usize>();
    assert!(
        retained_user_bytes > 64_000,
        "the independent item cap must not replace the 20k aggregate user budget"
    );
    assert!(
        retained_user_bytes <= 80_000,
        "retained user text exceeded the 20k aggregate budget: {retained_user_bytes} bytes"
    );
    assert!(history.iter().any(|envelope| {
        matches!(
            &envelope.item,
            ResponseItem::Message { content, .. }
                if content_items_to_text(content)
                    .is_some_and(|text| text.contains("recent-tail"))
        )
    }));
    assert!(matches!(
        history.last().map(|envelope| &envelope.item),
        Some(ResponseItem::Message { content, .. })
            if content_items_to_text(content)
                .is_some_and(|text| text.contains("summary-tail"))
    ));
}

#[test]
fn portable_compaction_caps_escaped_user_item_with_passthrough_metadata() {
    let escaped_text = format!("escaped-user:{}:escaped-user-tail", "\\\"\n".repeat(20_000));
    let mut item = user_message(&escaped_text);
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut item
    {
        *internal_chat_message_metadata_passthrough =
            Some(InternalChatMessageMetadataPassthrough {
                turn_id: Some("profile-a-routing-turn".to_string()),
                content_item_kinds: Some(vec![ContentItemKind("user.text".to_string())]),
                ..Default::default()
            });
    }
    let original = ResponseItemEnvelope {
        item,
        metadata: Some(CodexHarnessMetadata {
            execution_profile_id: Some("profile-a".to_string()),
            execution_generation: Some(7),
            ..CodexHarnessMetadata::default()
        }),
    };
    let history = build_compacted_history(
        Vec::new(),
        &collect_annotated_user_messages(std::slice::from_ref(&original)),
        "summary",
    );

    let user = &history[0];
    assert!(
        crate::context_manager::estimate_item_token_count(&user.item)
            <= MAX_PORTABLE_CONTEXT_ITEM_TOKENS as i64
    );
    assert_eq!(user.item.turn_id(), Some("profile-a-routing-turn"));
    assert!(matches!(
        &user.item,
        ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } if content_items_to_text(content)
            .is_some_and(|text| text.contains("escaped-user-tail"))
            && metadata
                .content_item_kinds
                .as_ref()
                .is_some_and(|kinds| !kinds.is_empty())
    ));
}

#[test]
fn portable_compaction_bounds_multipart_text_and_preserves_content_that_fits() {
    let make_item = |first: &str, second: &str| {
        serde_json::from_value::<ResponseItem>(json!({
            "type": "message", "role": "user",
            "content": [
                {"type": "input_text", "text": first},
                {"type": "input_text", "text": second},
            ],
            "internal_chat_message_metadata_passthrough": {
                "content_item_kinds": ["user.text", "user.text"],
            },
        }))
        .unwrap()
    };
    let short = make_item("first", "second");
    let large = make_item(&"a".repeat(20_000), &"b".repeat(20_000));
    let originals = [short.clone(), large];
    let history =
        build_compacted_history(Vec::new(), &collect_user_messages(&originals), "summary");

    assert_eq!(history[0].item, short);
    assert!(history.iter().all(|envelope| {
        crate::context_manager::estimate_item_token_count(&envelope.item)
            <= MAX_PORTABLE_CONTEXT_ITEM_TOKENS as i64
    }));
    assert!(matches!(
        &history[1].item,
        ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } if content.len() == 1 && metadata.content_item_kinds.as_ref().unwrap().len() == 1
    ));
}

#[test]
fn portable_compaction_caps_final_summary_after_turn_and_provenance_stamp() {
    let summary_text = format!(
        "escaped-summary:{}:escaped-summary-tail",
        "\\\"\n".repeat(20_000)
    );
    let mut history = build_compacted_history(Vec::new(), &[], &summary_text);
    let summary = history.last_mut().expect("compaction summary");
    summary.set_turn_id_if_missing("portable-compaction-turn");
    summary.metadata = Some(CodexHarnessMetadata {
        execution_profile_id: Some("profile-b".to_string()),
        execution_generation: Some(42),
        ..CodexHarnessMetadata::default()
    });
    bound_portable_context_item(&mut summary.item, MAX_PORTABLE_CONTEXT_ITEM_TOKENS);

    assert!(
        crate::context_manager::estimate_item_token_count(&summary.item)
            <= MAX_PORTABLE_CONTEXT_ITEM_TOKENS as i64
    );
    assert_eq!(summary.item.turn_id(), Some("portable-compaction-turn"));
    assert_eq!(
        summary.metadata,
        Some(CodexHarnessMetadata {
            execution_profile_id: Some("profile-b".to_string()),
            execution_generation: Some(42),
            ..CodexHarnessMetadata::default()
        })
    );
    assert!(matches!(
        &summary.item,
        ResponseItem::Message { content, .. }
            if content_items_to_text(content)
                .is_some_and(|text| text.contains("escaped-summary-tail"))
    ));
}

#[test]
fn build_compacted_history_preserves_user_message_passthrough_metadata() {
    let original = serde_json::from_value(json!({
        "type": "message", "role": "user",
        "id": ResponseItemId::with_suffix("msg", "user"),
        "content": [
            {"type": "input_image", "image_url": "file://image.png"},
            {"type": "input_text", "text": "first user message"},
            {"type": "input_audio", "audio_url": "file://audio.wav"}
        ],
        "internal_chat_message_metadata_passthrough": {
            "turn_id": "turn-1",
            "content_item_kinds": ["user.image", "user.text", "user.audio"]
        }
    }))
    .unwrap();
    let original = ResponseItemEnvelope {
        item: original,
        metadata: Some(CodexHarnessMetadata::default()),
    };
    let history = build_compacted_history(
        Vec::new(),
        &collect_annotated_user_messages(std::slice::from_ref(&original)),
        "summary text",
    );

    assert_eq!(
        history,
        vec![
            ResponseItemEnvelope {
                item: ResponseItem::Message {
                    id: Some(ResponseItemId::with_suffix("msg", "user")),
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "first user message".to_string(),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: Some(
                        InternalChatMessageMetadataPassthrough {
                            turn_id: Some("turn-1".to_string()),
                            content_item_kinds: Some(vec![ContentItemKind(
                                "user.text".to_string()
                            )]),
                            ..Default::default()
                        },
                    ),
                },
                metadata: Some(CodexHarnessMetadata::default()),
            },
            ResponseItemEnvelope::new(ContextualUserFragment::into(CompactionSummary::new(
                "summary text",
            ))),
        ]
    );
}

#[test]
fn insert_initial_context_before_last_real_user_or_summary_keeps_summary_last() {
    let agent_completion = ResponseItem::AgentMessage {
        id: None,
        author: "child".to_string(),
        recipient: "parent".to_string(),
        content: vec![AgentMessageInputContent::InputText {
            text: "Message Type: FINAL_ANSWER\nPayload:\nchild completion".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    };
    let compacted_history = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "latest user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_completion.clone(),
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let initial_context = vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];

    let refreshed = raw(insert_initial_context_before_last_real_user_or_summary(
        annotated(compacted_history),
        annotated(initial_context),
    ));
    let expected = vec![
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "older user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "fresh permissions".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "latest user".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_completion,
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!("{SUMMARY_PREFIX}\nsummary text"),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    assert_eq!(refreshed, expected);
}

#[test]
fn insert_initial_context_before_last_real_user_or_summary_keeps_compaction_last() {
    let agent_task = ResponseItem::AgentMessage {
        id: None,
        author: "parent".to_string(),
        recipient: "child".to_string(),
        content: Vec::new(),
        internal_chat_message_metadata_passthrough: None,
    };
    let compacted_history = vec![
        agent_task.clone(),
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "encrypted".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let initial_context = vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];

    let refreshed = raw(insert_initial_context_before_last_real_user_or_summary(
        annotated(compacted_history),
        annotated(initial_context),
    ));
    let expected = vec![
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "fresh permissions".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        },
        agent_task,
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "encrypted".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    assert_eq!(refreshed, expected);
}
