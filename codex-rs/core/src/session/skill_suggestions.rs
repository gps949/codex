//! Optional relevance hints use metadata without reading or invoking any skill.

use super::turn_context::TurnContext;
use crate::context::ContextualUserFragment;
use crate::context::SkillSuggestions;
use codex_model_provider::DECISION_ADVISOR_MAX_CANDIDATES;
use codex_model_provider::DECISION_ADVISOR_MAX_DESCRIPTION_BYTES;
use codex_model_provider::DecisionAdvice;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionCandidate;
use codex_model_provider::DecisionSearchRequest;
use codex_model_provider::DecisionSearchScope;
use codex_model_provider::decision_advisor;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use codex_skills::SkillMetadata;
use codex_skills::build_skill_name_counts;
use codex_skills::extract_tool_mentions;
use codex_skills_extension::SkillLoadOutcome;
use sha1::Digest;
use sha1::Sha1;
use tokio_util::sync::CancellationToken;

pub(super) async fn suggest(
    turn: &TurnContext,
    user_input: &[UserInput],
    current_injections: &[ResponseItem],
    explicitly_selected: &[SkillMetadata],
    cancellation: &CancellationToken,
) -> Option<ResponseItem> {
    let snapshot = turn.config.decision_advisor_snapshot().await.ok()?;
    let settings = &snapshot.effective;
    if settings.mode == DecisionAdvisorMode::Off
        || !settings.suggest_skills
        || !turn.config.include_skill_instructions
        || turn.session_source.is_non_root_agent()
        || cancellation.is_cancelled()
        || !explicitly_selected.is_empty()
        || user_input.iter().any(|input| match input {
            UserInput::Skill { .. } => true,
            UserInput::Text { text, .. } => !extract_tool_mentions(text).is_empty(),
            _ => false,
        })
        || current_injections.iter().any(|item| match item {
            ResponseItem::Message {
                internal_chat_message_metadata_passthrough,
                ..
            } => internal_chat_message_metadata_passthrough
                .as_ref()
                .and_then(|metadata| metadata.content_item_kinds.as_ref())
                .is_some_and(|kinds| {
                    kinds
                        .iter()
                        .any(|kind| kind.0 == "skills.selected_skill_instructions")
                }),
            _ => false,
        })
    {
        return None;
    }
    let query = user_input
        .iter()
        .rev()
        .find_map(|input| match input {
            UserInput::Text { text, .. } => Some(text.trim()),
            _ => None,
        })
        .filter(|query| !query.is_empty() && query.len() <= 2048)?;
    let snapshot = turn.skills_snapshot();
    let (candidates, names, baseline, revision) =
        candidates(snapshot.outcome(), &turn.session_source, query);
    if candidates.is_empty() {
        return None;
    }
    let credential = turn.config.decision_advisor_credential(settings).ok()?;
    let factory = turn.config.http_client_factory();
    let advisor = decision_advisor();
    let advice = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return None,
        advice = advisor.rank(settings,&factory,DecisionSearchRequest {
            scope:DecisionSearchScope::Skills,query,candidates:&candidates,catalog_revision:&revision,
        },credential.as_ref().map(codex_model_provider::DecisionAdvisorSecret::expose_secret)) => advice,
    };
    let DecisionAdvice::Ranked(ids) = advice else {
        return None;
    };
    // Choice probabilities establish confidence in the best option; do not infer independent
    // confidence for a second skill from a small residual probability.
    let selected = ids
        .first()
        .and_then(|id| candidates.iter().position(|candidate| &candidate.id == id))?;
    advisor.record_comparison(&baseline, &[selected]);
    if settings.mode == DecisionAdvisorMode::Shadow {
        return None;
    }
    let fragment = SkillSuggestions::new(vec![names[selected].clone()])?;
    advisor.record_application();
    Some(ContextualUserFragment::into(fragment))
}

type CandidateSelection = (Vec<DecisionCandidate>, Vec<String>, Vec<usize>, [u8; 20]);

fn candidates(
    outcome: &SkillLoadOutcome,
    source: &SessionSource,
    query: &str,
) -> CandidateSelection {
    let (_, name_counts) = build_skill_name_counts(&outcome.skills, &outcome.disabled_paths);
    let eligible = |skill: &&SkillMetadata| {
        outcome.is_skill_enabled(skill)
            && skill.allows_implicit_invocation()
            && skill.matches_product_restriction_for_product(source.restriction_product())
            && !skill.name.is_empty()
            && skill.name.len() <= 128
            && !skill.name.chars().any(char::is_control)
            && name_counts.get(&skill.name.to_ascii_lowercase()) == Some(&1)
    };
    let count = outcome.skills.iter().filter(eligible).count();
    let terms = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(16)
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let mut lexical = Vec::new();
    let mut sampled = Vec::new();
    let mut digest = Sha1::new();
    let sample_slots = if count <= DECISION_ADVISOR_MAX_CANDIDATES {
        count
    } else {
        8
    };
    let mut sample_position = 0;
    for (index, skill) in outcome.skills.iter().filter(eligible).enumerate() {
        digest.update(skill.name.len().to_be_bytes());
        digest.update(skill.name.as_bytes());
        digest.update(skill.description.len().to_be_bytes());
        digest.update(skill.description.as_bytes());
        let description = descriptor(skill);
        let lower = description.to_lowercase();
        let score = terms
            .iter()
            .filter(|term| lower.contains(term.as_str()))
            .count();
        if score > 0 {
            lexical.push((score, index, skill));
            lexical.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
            lexical.truncate(24);
        }
        if sample_position < sample_slots
            && index
                == sample_position * count.saturating_sub(1) / sample_slots.saturating_sub(1).max(1)
        {
            sampled.push((index, skill));
            sample_position += 1;
        }
    }
    let best_lexical = lexical.first().map(|(_, index, _)| *index);
    let mut selected = lexical
        .into_iter()
        .map(|(_, index, skill)| (index, skill))
        .collect::<Vec<_>>();
    for (index, skill) in sampled {
        if selected.len() == DECISION_ADVISOR_MAX_CANDIDATES {
            break;
        }
        if !selected.iter().any(|(existing, _)| *existing == index) {
            selected.push((index, skill));
        }
    }
    let baseline = best_lexical
        .and_then(|best| selected.iter().position(|(index, _)| *index == best))
        .into_iter()
        .collect();
    let candidates = selected
        .iter()
        .map(|(index, skill)| DecisionCandidate {
            id: format!("s{index}"),
            description: descriptor(skill),
        })
        .collect();
    let names = selected
        .into_iter()
        .map(|(_, skill)| skill.name.clone())
        .collect();
    (candidates, names, baseline, digest.finalize().into())
}

fn descriptor(skill: &SkillMetadata) -> String {
    let text = format!("{}: {}", skill.name, skill.description);
    let mut end = text.len().min(DECISION_ADVISOR_MAX_DESCRIPTION_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
#[path = "skill_suggestions_tests.rs"]
mod tests;
