//! Checks retained modalities before projection and estimates fresh model-visible input.

use crate::context_manager::estimate_image_reference_bytes;
use crate::utils::json::serialized_json_bytes;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use codex_utils_output_truncation::approx_tokens_from_byte_count_i64;

const IMAGE_INPUT_MARGIN_TOKENS: i64 = 4096;

#[derive(Default)]
pub(crate) struct RetainedMediaRequirements {
    pub(crate) images: bool,
    pub(crate) audio: bool,
}

pub(crate) fn retained_media_requirements<'a>(
    items: impl Iterator<Item = &'a ResponseItem>,
) -> RetainedMediaRequirements {
    let mut media = RetainedMediaRequirements::default();
    for item in items {
        match item {
            ResponseItem::Message { content, .. } => {
                for part in content {
                    match part {
                        ContentItem::InputImage { .. } => media.images = true,
                        ContentItem::InputAudio { .. } => media.audio = true,
                        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => {}
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. }
            | ResponseItem::CustomToolCallOutput { output, .. } => {
                for part in output.content_items().into_iter().flatten() {
                    match part {
                        FunctionCallOutputContentItem::InputImage { .. } => media.images = true,
                        FunctionCallOutputContentItem::InputAudio { .. } => media.audio = true,
                        FunctionCallOutputContentItem::InputText { .. }
                        | FunctionCallOutputContentItem::EncryptedContent { .. } => {}
                    }
                }
            }
            ResponseItem::ImageGenerationCall { .. } => media.images = true,
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ConfigurationUpdate { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {}
        }
    }
    media
}

pub(crate) fn estimate_fresh_input_tokens(content: &[UserInput]) -> i64 {
    content
        .iter()
        .map(|input| {
            let image_bytes = match input {
                UserInput::Image { image, detail } => {
                    Some(estimate_image_reference_bytes(image, *detail))
                }
                // Local image dimensions are unavailable at admission. Reuse the reference
                // estimator's conservative original-detail maximum without reading the file.
                UserInput::LocalImage { detail, .. } => Some(estimate_image_reference_bytes(
                    &ImageReference::File {
                        file_id: String::new(),
                    },
                    *detail,
                )),
                _ => None,
            };
            if let Some(bytes) = image_bytes {
                return approx_tokens_from_byte_count_i64(bytes)
                    .saturating_add(IMAGE_INPUT_MARGIN_TOKENS);
            }
            // Byte counts conservatively cover fresh text and structured mentions without
            // retaining another serialized buffer. Image encodings never enter this estimate.
            serialized_json_bytes(input)
                .map_or(i64::MAX, |bytes| i64::try_from(bytes).unwrap_or(i64::MAX))
        })
        .fold(0i64, i64::saturating_add)
}
