//! Request shape for standby 5h-window warmup.
//!
//! Interactive ChatGPT turns start the 5h window with the model's real
//! instructions plus the same tool harness the session would advertise for that
//! model. Current ChatGPT slugs are `code_mode_only` and expect `exec`/`wait`,
//! not the classic `exec_command` CLI set. Warmup reuses that payload without
//! creating a Session or calling activate/lease.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::client_common::Prompt;
use crate::config::Config;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::tools::code_mode::execute_spec::create_code_mode_tool;
use crate::tools::code_mode::wait_spec::create_wait_tool;
use crate::tools::handlers::apply_patch_spec::create_apply_patch_freeform_tool;
use crate::tools::handlers::plan_spec::create_update_plan_tool;
use crate::tools::handlers::shell_spec::CommandToolOptions;
use crate::tools::handlers::shell_spec::create_exec_command_tool_with_environment_id;
use crate::tools::handlers::shell_spec::create_write_stdin_tool;
use crate::tools::handlers::view_image_spec::ViewImageToolOptions;
use crate::tools::handlers::view_image_spec::create_view_image_tool;
use codex_code_mode::ImageDetailVisibility;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::ThreadSource;
use codex_tools::ToolSpec;

const WARMUP_PROMPT: &str = "1+1?";

pub(crate) fn warmup_prompt(model_info: &ModelInfo, config: &Config) -> Prompt {
    Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: WARMUP_PROMPT.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: warmup_tool_specs(model_info),
        parallel_tool_calls: true,
        base_instructions: warmup_base_instructions(model_info, config),
        ..Default::default()
    }
}

pub(crate) fn warmup_responses_metadata(
    installation_id: String,
    thread_id: ThreadId,
) -> CodexResponsesMetadata {
    let turn_id = uuid::Uuid::now_v7().to_string();
    CodexResponsesMetadata {
        request_kind: Some(CodexResponsesRequestKind::Turn),
        turn_id: Some(turn_id.clone()),
        root_turn_id: Some(turn_id),
        thread_source: Some(ThreadSource::User),
        ..CodexResponsesMetadata::new(
            installation_id,
            thread_id.to_string(),
            thread_id.to_string(),
            format!("{thread_id}:warmup"),
        )
    }
}

fn warmup_base_instructions(model_info: &ModelInfo, config: &Config) -> BaseInstructions {
    let text = model_info.get_model_instructions(config.personality);
    if text.trim().is_empty() {
        return BaseInstructions::default();
    }
    BaseInstructions {
        text,
        provenance: Some(BaseInstructionsProvenance::Model {
            model: model_info.slug.clone(),
        }),
    }
}

fn warmup_tool_specs(model_info: &ModelInfo) -> Arc<[ToolSpec]> {
    match model_info.tool_mode {
        Some(ToolMode::CodeMode | ToolMode::CodeModeOnly) => Arc::from([
            create_code_mode_tool(
                &[],
                &[],
                &BTreeMap::new(),
                codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
                matches!(model_info.tool_mode, Some(ToolMode::CodeModeOnly)),
                ImageDetailVisibility::Visible,
            ),
            create_wait_tool(),
        ]),
        Some(ToolMode::Direct) | None => Arc::from([
            create_exec_command_tool_with_environment_id(
                CommandToolOptions {
                    allow_login_shell: false,
                    exec_permission_approvals_enabled: false,
                },
                /*include_environment_id*/ false,
                /*include_shell_parameter*/ true,
                /*include_windows_shell_guidance*/ cfg!(windows),
            ),
            create_write_stdin_tool(),
            create_apply_patch_freeform_tool(/*include_environment_id*/ false),
            create_update_plan_tool(),
            create_view_image_tool(ViewImageToolOptions {
                can_request_original_image_detail: false,
                unified_image_budget: false,
                include_environment_id: false,
            }),
        ]),
    }
}
