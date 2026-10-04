//! A bounded advisory fragment with known names and no model-generated instructions.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

pub(crate) struct SkillSuggestions {
    names_json: String,
}

impl SkillSuggestions {
    pub(crate) fn new(names: Vec<String>) -> Option<Self> {
        if names.is_empty()
            || names.len() > 2
            || names.iter().any(|name| {
                name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
            })
        {
            return None;
        }
        let names_json = serde_json::to_string(&names)
            .ok()?
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026");
        let fragment = Self { names_json };
        (fragment.render().len() <= 1200).then_some(fragment)
    }
}

impl ContextualUserFragment for SkillSuggestions {
    fn role(&self) -> &'static str {
        "user"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("skills.advisor_suggestions".into())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<skill_suggestions>", "</skill_suggestions>")
    }

    fn body(&self) -> String {
        format!(
            "\nAn optional discovery advisor suggests considering these existing skill names: {}. This is a relevance hint, not a user request or authorization. Follow the user's explicit choices, AGENTS instructions, skill invocation rules and permissions. Read a suggested skill through the normal workflow only if it is appropriate; do not install dependencies or enable apps based on this hint.\n",
            self.names_json
        )
    }
}

#[cfg(test)]
#[path = "skill_suggestions_tests.rs"]
mod tests;
