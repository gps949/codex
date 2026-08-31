use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// Fixed-size notice used when account-scoped encrypted tool output cannot cross an execution
/// account boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AccountTransitionToolOutputNotice;

impl ContextualUserFragment for AccountTransitionToolOutputNotice {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("account_pool.tool_output_omitted".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            "<account_transition_tool_output_notice>",
            "</account_transition_tool_output_notice>",
        )
    }

    fn body(&self) -> String {
        "\nEncrypted tool output was omitted because it belongs to a different Codex execution account.\n"
            .to_string()
    }
}
