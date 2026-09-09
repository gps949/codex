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
use codex_backend_client::Client as BackendClient;
use codex_login::AccountPool;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
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
const FAILURE_BACKOFF: Duration = Duration::from_secs(15 * 60);
const SUCCESS_DEBOUNCE: Duration = Duration::from_secs(5 * 60);
const SKIPPED_AUTH_BACKOFF: Duration = Duration::from_secs(30 * 60);
const GET_REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
const URGENT_WARMUP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Spawns the periodic standby-window warmup loop. The caller owns the handle and may abort it.
pub(crate) fn spawn_window_warmup_task(pool: Arc<AccountPool>, config: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Settle install/quota probes before the first pass.
        tokio::time::sleep(
            config
                .account_pool
                .effective_window_warmup_interval()
                .min(URGENT_WARMUP_INTERVAL),
        )
        .await;
        loop {
            if !config.account_pool.effective_window_warmup() {
                tokio::time::sleep(config.account_pool.effective_window_warmup_interval()).await;
                continue;
            }
            if let Err(error) = run_warmup_pass(&pool, &config).await {
                debug!(error = %error, "account window warmup pass failed");
            }
            let sleep_for = if pool.needs_urgent_window_warmup(
                config.account_pool.effective_preemptive_switch_percent(),
            ) {
                URGENT_WARMUP_INTERVAL.min(config.account_pool.effective_window_warmup_interval())
            } else {
                config.account_pool.effective_window_warmup_interval()
            };
            tokio::time::sleep(sleep_for).await;
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
    let attempted_at = Utc::now();
    let Some(auth) = auth_manager.auth().await.filter(CodexAuth::is_chatgpt_auth) else {
        debug!(%profile_id, "skipping window warmup without ChatGPT auth");
        let _ = pool.record_window_warmup(
            profile_id,
            WindowWarmupObservation {
                outcome: WindowWarmupOutcome::SkippedNoAuth,
                attempted_at,
                retry_after: Some(
                    attempted_at + chrono::Duration::seconds(SKIPPED_AUTH_BACKOFF.as_secs() as i64),
                ),
            },
        );
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
        Some(Arc::clone(&auth_manager)),
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

    let stream_result = tokio::time::timeout(PER_PROFILE_TIMEOUT, warm).await;
    let stream_limits = match stream_result {
        Ok(Ok(observed)) => observed,
        Ok(Err(error)) => {
            record_failure(pool, profile_id, attempted_at);
            warn!(%profile_id, error = %error, "standby window warmup request failed");
            return Ok(());
        }
        Err(_elapsed) => {
            record_failure(pool, profile_id, attempted_at);
            warn!(%profile_id, "standby window warmup timed out");
            return Ok(());
        }
    };

    if let Some(snapshot) = stream_limits {
        pool.update_rate_limits(profile_id, convert_rate_limits(&snapshot))?;
        debug!(%profile_id, "warmed standby 5h rate-limit window");
    } else {
        debug!(%profile_id, "warmup completed without rate-limit headers");
    }

    // Prefer the accounts usage GET as the durable observation when stream headers are missing,
    // and always refresh after a successful kick so standby quota stays current for scheduling.
    if let Some(limits) = refresh_rate_limits_via_get(config, &auth).await {
        pool.update_rate_limits(profile_id, limits)?;
        debug!(%profile_id, "refreshed standby rate limits after window warmup");
    }

    let _ = pool.record_window_warmup(
        profile_id,
        WindowWarmupObservation {
            outcome: WindowWarmupOutcome::Succeeded,
            attempted_at,
            retry_after: Some(
                attempted_at + chrono::Duration::seconds(SUCCESS_DEBOUNCE.as_secs() as i64),
            ),
        },
    );
    Ok(())
}

fn record_failure(pool: &AccountPool, profile_id: &AccountProfileId, attempted_at: DateTime<Utc>) {
    let retry_after = attempted_at + chrono::Duration::seconds(FAILURE_BACKOFF.as_secs() as i64);
    let _ = pool.record_window_warmup(
        profile_id,
        WindowWarmupObservation {
            outcome: WindowWarmupOutcome::Failed,
            attempted_at,
            retry_after: Some(retry_after),
        },
    );
}

async fn refresh_rate_limits_via_get(
    config: &Config,
    auth: &CodexAuth,
) -> Option<AccountRateLimits> {
    let client = BackendClient::from_auth(
        config.chatgpt_base_url.clone(),
        auth,
        config.http_client_factory(),
    );
    let observed_at = Utc::now();
    let snapshots = tokio::time::timeout(GET_REFRESH_TIMEOUT, client.get_rate_limits_many())
        .await
        .ok()?
        .ok()?;
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| snapshots.first())?;
    Some(AccountRateLimits {
        primary: snapshot.primary.as_ref().map(convert_rate_limit_window),
        secondary: snapshot.secondary.as_ref().map(convert_rate_limit_window),
        observed_at: Some(observed_at),
    })
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
