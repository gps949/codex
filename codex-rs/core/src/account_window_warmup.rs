//! Identity-preserving primary (5h) window warmup for standby account-pool profiles.
//!
//! ChatGPT's 5h window does not start ticking while usage stays at 0%. Under `fill_first`,
//! backup accounts can sit idle indefinitely until failover. This task sends a tiny generating
//! Responses request with each standby profile's own AuthManager without calling `activate` /
//! `lease`, so the active execution identity and prompt cache stay put.

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use codex_api::ResponseEvent;
use codex_login::AccountPool;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AuthManager;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::SessionSource;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::warn;

use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::config::Config;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use codex_rollout_trace::InferenceTraceContext;

const WARMUP_PROMPT: &str = "ok";
const WARMUP_INSTRUCTIONS: &str = "Reply with ok.";
const WARMUP_ORIGINATOR: &str = "codex_account_window_warmup";
const DEFAULT_WARMUP_MODEL: &str = "gpt-5.2";
const PER_PROFILE_TIMEOUT: Duration = Duration::from_secs(45);

/// Spawns the periodic standby-window warmup loop. The caller owns the handle and may abort it.
pub(crate) fn spawn_window_warmup_task(pool: Arc<AccountPool>, config: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = config.account_pool.effective_window_warmup_interval();
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick is immediate; skip so install/quota probes settle first.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if !config.account_pool.effective_window_warmup() {
                continue;
            }
            if let Err(error) = run_warmup_pass(&pool, &config).await {
                debug!(error = %error, "account window warmup pass failed");
            }
        }
    })
}

async fn run_warmup_pass(pool: &AccountPool, config: &Config) -> anyhow::Result<()> {
    let Some(profile_id) = pool.window_warmup_candidates().into_iter().next() else {
        return Ok(());
    };
    let Some((_, auth_manager)) = pool
        .auth_managers()
        .into_iter()
        .find(|(id, _)| id == &profile_id)
    else {
        return Ok(());
    };
    warm_profile(pool, config, &profile_id, auth_manager).await
}

async fn warm_profile(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
) -> anyhow::Result<()> {
    let Some(_auth) = auth_manager
        .auth()
        .await
        .filter(codex_login::CodexAuth::is_chatgpt_auth)
    else {
        debug!(%profile_id, "skipping window warmup without ChatGPT auth");
        return Ok(());
    };

    let mut provider = config.model_provider.clone();
    // Force HTTP so warmup never shares or perturbs the active session's websocket.
    provider.supports_websockets = false;

    let model = config
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_WARMUP_MODEL.to_string());
    let model_info = warmup_model_info(&model);
    let thread_id = ThreadId::new();
    let client = ModelClient::new(
        Some(auth_manager),
        AgentIdentityAuthPolicy::ChatGptAuth,
        thread_id,
        provider,
        SessionSource::Cli,
        WARMUP_ORIGINATOR.to_string(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        config.http_client_factory(),
    );
    let session_telemetry = SessionTelemetry::new(
        thread_id,
        &model_info.slug,
        &model_info.slug,
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        WARMUP_ORIGINATOR.to_string(),
        /*log_user_prompts*/ false,
        "account-window-warmup".to_string(),
        SessionSource::Cli,
    );
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: WARMUP_PROMPT.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: WARMUP_INSTRUCTIONS.to_string(),
            provenance: None,
        },
        ..Default::default()
    };
    let responses_metadata = CodexResponsesMetadata {
        request_kind: Some(CodexResponsesRequestKind::Turn),
        ..CodexResponsesMetadata::new(
            "account-window-warmup".to_string(),
            thread_id.to_string(),
            thread_id.to_string(),
            format!("{thread_id}:warmup"),
        )
    };

    let warm = async {
        let mut session = client.new_session();
        let mut stream = session
            .stream(
                &prompt,
                &model_info,
                &session_telemetry,
                /*effort*/ None,
                ReasoningSummary::None,
                /*service_tier*/ None,
                &responses_metadata,
                &InferenceTraceContext::disabled(),
            )
            .await?;
        let mut observed = None;
        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::RateLimits(snapshot) => {
                    observed = Some(snapshot);
                }
                ResponseEvent::Completed { .. } => break,
                _ => {}
            }
        }
        anyhow::Ok(observed)
    };

    match tokio::time::timeout(PER_PROFILE_TIMEOUT, warm).await {
        Ok(Ok(Some(snapshot))) => {
            pool.update_rate_limits(profile_id, convert_rate_limits(&snapshot))?;
            debug!(%profile_id, "warmed standby 5h rate-limit window");
        }
        Ok(Ok(None)) => {
            debug!(%profile_id, "warmup completed without rate-limit headers");
        }
        Ok(Err(error)) => {
            warn!(%profile_id, error = %error, "standby window warmup request failed");
        }
        Err(_elapsed) => {
            warn!(%profile_id, "standby window warmup timed out");
        }
    }
    Ok(())
}

fn convert_rate_limits(snapshot: &RateLimitSnapshot) -> AccountRateLimits {
    AccountRateLimits {
        primary: snapshot.primary.as_ref().map(convert_rate_limit_window),
        secondary: snapshot.secondary.as_ref().map(convert_rate_limit_window),
        observed_at: Some(Utc::now()),
    }
}

fn convert_rate_limit_window(window: &RateLimitWindow) -> AccountRateLimitWindow {
    AccountRateLimitWindow {
        used_percent: window.used_percent,
        resets_at: window
            .resets_at
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
    }
}

fn warmup_model_info(model: &str) -> ModelInfo {
    serde_json::from_value(serde_json::json!({
        "slug": model,
        "display_name": model,
        "description": "account pool window warmup",
        "default_reasoning_level": "minimal",
        "supported_reasoning_levels": [
            {"effort": "minimal", "description": "minimal"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "model_messages": null,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_image_detail_original": false,
        "context_window": 32000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .unwrap_or_else(|error| {
        panic!("warmup model info for {model} must deserialize: {error}");
    })
}

#[cfg(test)]
#[path = "account_window_warmup_tests.rs"]
mod tests;
