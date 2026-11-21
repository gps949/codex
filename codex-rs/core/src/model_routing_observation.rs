//! Bounded local observations of committed selections and unapplied proposals.

use crate::config::Config;
use crate::task_model_routing::RoutingScope;
use crate::task_model_routing::TaskRoutingDecision;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;
use std::io::Read;
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
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(".model-routing-observation.lock"))?;
        lock.lock()?;
        let history_path = directory.join("model-routing-history.json");
        let mut previous = Vec::new();
        if let Ok(file) = std::fs::File::open(&history_path) {
            file.take(262_145).read_to_end(&mut previous)?;
        }
        let mut history: Vec<serde_json::Value> = if previous.len() <= 262_144 {
            serde_json::from_slice(&previous).unwrap_or_default()
        } else {
            Vec::new()
        };
        // Malformed entries must not retain arbitrary content in the observation history.
        history.retain(|entry| {
            entry.as_object().is_some_and(|fields| {
                fields.len() == 7
                    && fields.keys().all(|key| {
                        matches!(
                            key.as_str(),
                            "model"
                                | "effort"
                                | "source"
                                | "scope"
                                | "applied"
                                | "decidedAt"
                                | "reason"
                        )
                    })
                    && serde_json::to_vec(entry).is_ok_and(|bytes| bytes.len() <= 4096)
            })
        });
        if history.len() >= 64 {
            history.drain(..history.len() - 63);
        }
        history.push(serde_json::from_slice(&bytes)?);
        let mut recent = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer(&mut recent, &history)?;
        recent.as_file().sync_all()?;
        recent.persist(history_path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_model_provider::RoutingSelection;
    use pretty_assertions::assert_eq;

    fn decision(index: usize) -> TaskRoutingDecision {
        TaskRoutingDecision {
            selection: RoutingSelection {
                model: format!("model-{index}"),
                effort: Some(ReasoningEffort::Low),
                reason: "Task complexity: simple; model role: economy.".into(),
            },
            apply: true,
            source: "Local rules".into(),
        }
    }

    #[tokio::test]
    async fn recent_history_retains_newest_selections_and_latest_compatibility()
    -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let config = crate::config::ConfigBuilder::default()
            .codex_home(home.path().into())
            .build()
            .await?;
        for index in 0..70 {
            record(
                &config,
                &decision(index),
                RoutingScope::Main,
                ObservationApplication::Committed,
            )
            .await;
        }
        let history: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(
            home.path().join("model-routing-history.json"),
        )?)?;
        assert_eq!(history.len(), 64);
        assert_eq!(history[0]["model"], "model-6");
        assert_eq!(history[63]["model"], "model-69");
        let latest: serde_json::Value = serde_json::from_slice(&std::fs::read(
            home.path().join("model-routing-observation.json"),
        )?)?;
        assert_eq!(history.last(), Some(&latest));
        assert_eq!(latest.as_object().unwrap().len(), 7);
        assert!(latest.get("task").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_observations_are_retained_and_corrupt_history_recovers()
    -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("model-routing-history.json"), "invalid")?;
        let config = std::sync::Arc::new(
            crate::config::ConfigBuilder::default()
                .codex_home(home.path().into())
                .build()
                .await?,
        );
        let mut writers = tokio::task::JoinSet::new();
        for index in 0..16 {
            let config = config.clone();
            writers.spawn(async move {
                record(
                    &config,
                    &decision(index),
                    RoutingScope::Subagent,
                    ObservationApplication::Proposal,
                )
                .await;
            });
        }
        while let Some(result) = writers.join_next().await {
            result?;
        }
        let history: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(
            home.path().join("model-routing-history.json"),
        )?)?;
        assert_eq!(history.len(), 16);
        let models = history
            .iter()
            .map(|entry| entry["model"].as_str().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(models.len(), 16);
        assert!(history.iter().all(|entry| entry["applied"] == false));
        assert!(history.iter().all(|entry| entry["scope"] == "subagent"));
        Ok(())
    }
}
