//! Semantic discovery changes only the returned search candidates.

use super::tool_search::ToolSearchHandler;
use crate::tools::context::ToolInvocation;
use codex_model_provider::DECISION_ADVISOR_MAX_CANDIDATES;
use codex_model_provider::DECISION_ADVISOR_MAX_DESCRIPTION_BYTES;
use codex_model_provider::DecisionAdvice;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionCandidate;
use codex_model_provider::DecisionSearchRequest;
use codex_model_provider::DecisionSearchScope;
use codex_model_provider::decision_advisor;
use codex_tools::ToolSearchInfo;
use sha1::Digest;
use sha1::Sha1;

pub(super) fn catalog_revision(infos: &[ToolSearchInfo]) -> [u8; 20] {
    let mut digest = Sha1::new();
    for info in infos {
        digest.update(info.entry.search_text.len().to_be_bytes());
        digest.update(info.entry.search_text.as_bytes());
    }
    digest.finalize().into()
}

pub(super) async fn ranked_ids(
    handler: &ToolSearchHandler,
    invocation: &ToolInvocation,
    query: &str,
    baseline: &[usize],
    limit: usize,
) -> Vec<usize> {
    let config = &invocation.step_context.turn.config;
    let settings = &config.decision_advisor;
    if settings.mode == DecisionAdvisorMode::Off || invocation.cancellation_token.is_cancelled() {
        return baseline.to_vec();
    }
    let mut indices = handler
        .search_engine
        .search(query, DECISION_ADVISOR_MAX_CANDIDATES)
        .into_iter()
        .map(|result| result.document.id)
        .collect::<Vec<_>>();
    // Small catalogs can be considered in full even when lexical search misses synonyms or
    // Chinese queries. Crowded catalogs reserve a few slots for deterministic catalog coverage.
    if handler.search_infos.len() > DECISION_ADVISOR_MAX_CANDIDATES {
        indices.truncate(DECISION_ADVISOR_MAX_CANDIDATES - 8);
    }
    let count = handler.search_infos.len();
    let coverage_count = if count <= DECISION_ADVISOR_MAX_CANDIDATES {
        count
    } else {
        DECISION_ADVISOR_MAX_CANDIDATES - indices.len()
    };
    for position in 0..coverage_count {
        let index = if count <= DECISION_ADVISOR_MAX_CANDIDATES {
            position
        } else {
            position * (count - 1) / (coverage_count - 1).max(1)
        };
        if !indices.contains(&index) {
            indices.push(index);
        }
        if indices.len() == DECISION_ADVISOR_MAX_CANDIDATES {
            break;
        }
    }
    let candidates = indices
        .iter()
        .map(|index| {
            let text = &handler.search_infos[*index].entry.search_text;
            let mut end = text.len().min(DECISION_ADVISOR_MAX_DESCRIPTION_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            DecisionCandidate {
                id: format!("t{index}"),
                description: text[..end].to_string(),
            }
        })
        .collect::<Vec<_>>();
    let credential = (!settings.api_key_env.is_empty())
        .then(|| std::env::var(&settings.api_key_env).ok())
        .flatten();
    let advisor = decision_advisor();
    let factory = config.http_client_factory();
    let advice = tokio::select! {
        biased;
        _ = invocation.cancellation_token.cancelled() => return baseline.to_vec(),
        advice = advisor.rank(settings,&factory,DecisionSearchRequest {
            scope: DecisionSearchScope::Tools,
            query,candidates:&candidates,catalog_revision:&handler.catalog_revision,
        },credential.as_deref()) => advice,
    };
    let DecisionAdvice::Ranked(ids) = advice else {
        return baseline.to_vec();
    };
    let mut ranked = ids
        .into_iter()
        .filter_map(|id| {
            candidates
                .iter()
                .position(|candidate| candidate.id == id)
                .map(|position| indices[position])
        })
        .take(limit)
        .collect::<Vec<_>>();
    for index in baseline {
        if ranked.len() == limit {
            break;
        }
        if !ranked.contains(index) {
            ranked.push(*index);
        }
    }
    advisor.record_comparison(baseline, &ranked);
    if settings.mode == DecisionAdvisorMode::Shadow {
        return baseline.to_vec();
    }
    if ranked != baseline {
        advisor.record_application();
    }
    ranked
}
