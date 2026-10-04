//! Inspect or deliberately probe the optional tool-discovery advisor.

use clap::Args;
use clap::Subcommand;
use codex_core::config::ConfigBuilder;
use codex_model_provider::DecisionAdvice;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionCandidate;
use codex_model_provider::DecisionSearchRequest;
use codex_model_provider::DecisionSearchScope;
use codex_model_provider::decision_advisor;
use codex_utils_cli::CliConfigOverrides;
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(crate) struct DecisionAdvisorCommand {
    #[clap(flatten)]
    pub(crate) config_overrides: CliConfigOverrides,
    #[command(subcommand)]
    action: AdvisorAction,
}

#[derive(Debug, Subcommand)]
enum AdvisorAction {
    /// Show configuration and credential presence without making a network request.
    Status,
    /// Send an explicit test query and catalog to the configured independent service.
    Probe {
        #[arg(long)]
        query: String,
        /// JSON array of objects with name and description fields (maximum 32 candidates).
        #[arg(long)]
        catalog: PathBuf,
    },
}

#[derive(Deserialize)]
struct ProbeTool {
    name: String,
    description: String,
}

pub(crate) async fn run(command: DecisionAdvisorCommand) -> anyhow::Result<()> {
    let config = ConfigBuilder::default()
        .cli_overrides(
            command
                .config_overrides
                .parse_overrides()
                .map_err(anyhow::Error::msg)?,
        )
        .build()
        .await?;
    let snapshot = config.decision_advisor_snapshot().await?;
    let settings = &snapshot.effective;
    let credential = config.decision_advisor_credential(settings)?;
    match command.action {
        AdvisorAction::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "mode":settings.mode,"provider":settings.provider,"model":settings.model,
                    "suggestSkills": settings.suggest_skills,
                    "endpointConfigured":!settings.endpoint.is_empty(),"apiKeyEnv":settings.api_key_env,
                    "credentialPresent":credential.as_ref().is_some_and(|value| !value.expose_secret().is_empty()),
                    "credentialSource":settings.credential_source,
                    "overridden":snapshot.overridden,
                    "timeoutMs":settings.timeout.as_millis(),"minConfidence":settings.min_confidence,
                    "scope":"tool_search ranking and independently enabled skill hints",
                    "diagnostics":"Anonymous runtime counters appear in debug logs; counters are process-local.",
                }))?
            );
        }
        AdvisorAction::Probe { query, catalog } => {
            if settings.mode == DecisionAdvisorMode::Off {
                anyhow::bail!(
                    "The decision advisor is off. Explicitly configure shadow or rank before sending a probe."
                );
            }
            let mut bytes = Vec::new();
            std::fs::File::open(catalog)?
                .take(32769)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 32768 {
                anyhow::bail!("The probe catalog exceeds 32 KiB.");
            }
            let tools: Vec<ProbeTool> = serde_json::from_slice(&bytes)?;
            if tools.is_empty()
                || tools.len() > codex_model_provider::DECISION_ADVISOR_MAX_CANDIDATES
            {
                anyhow::bail!("The probe catalog must contain between 1 and 32 tools.");
            }
            let candidates = tools
                .iter()
                .enumerate()
                .map(|(index, tool)| DecisionCandidate {
                    id: format!("t{index}"),
                    description: format!("{}: {}", tool.name, tool.description),
                })
                .collect::<Vec<_>>();
            let advisor = decision_advisor();
            let result = advisor
                .rank(
                    settings,
                    &config.http_client_factory(),
                    DecisionSearchRequest {
                        scope: DecisionSearchScope::Tools,
                        query: &query,
                        candidates: &candidates,
                        catalog_revision: b"explicit-cli-probe",
                    },
                    credential
                        .as_ref()
                        .map(codex_model_provider::DecisionAdvisorSecret::expose_secret),
                )
                .await;
            let names = match &result {
                DecisionAdvice::Ranked(ids) => ids
                    .iter()
                    .filter_map(|id| {
                        candidates
                            .iter()
                            .position(|candidate| &candidate.id == id)
                            .map(|index| tools[index].name.clone())
                    })
                    .collect::<Vec<_>>(),
                DecisionAdvice::Fallback(_) => Vec::new(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"result":result,"ranking":names,"stats":advisor.stats()})
                )?
            );
        }
    }
    Ok(())
}
