//! Bounded local observations of committed selections and unapplied proposals.

use crate::config::Config;
use crate::task_model_routing::RoutingScope;
use crate::task_model_routing::TaskRoutingDecision;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;
use std::io::Write;

pub(crate) enum ObservationApplication {
    Committed,
    Proposal,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation<'a> {
    model: &'a str,
    effort: Option<&'a ReasoningEffort>,
    source: &'a str,
    scope: &'a str,
    applied: bool,
    decided_at: i64,
    reason: &'a str,
}

pub(crate) async fn record(
    config: &Config,
    decision: &TaskRoutingDecision,
    scope: RoutingScope,
    application: ObservationApplication,
) {
    let mut end = decision.selection.reason.len().min(512);
    while !decision.selection.reason.is_char_boundary(end) {
        end -= 1;
    }
    let observation = Observation {
        model: &decision.selection.model,
        effort: decision.selection.effort.as_ref(),
        source: &decision.source,
        scope: match scope {
            RoutingScope::Main => "main",
            RoutingScope::Subagent => "subagent",
        },
        applied: matches!(application, ObservationApplication::Committed),
        decided_at: chrono::Utc::now().timestamp(),
        reason: &decision.selection.reason[..end],
    };
    let Ok(bytes) = serde_json::to_vec(&observation) else {
        return;
    };
    if bytes.len() > 4096 {
        return;
    }
    let directory = config.codex_home.to_path_buf();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary.persist(directory.join("model-routing-observation.json"))?;
        Ok(())
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        tracing::debug!("Could not save the latest model routing observation");
    }
}
