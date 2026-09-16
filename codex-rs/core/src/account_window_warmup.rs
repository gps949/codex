//! Identity-preserving primary (5h) window warmup for standby account-pool profiles.
//!
//! ChatGPT's 5h window does not start ticking while usage stays at 0%. Under `fill_first`,
//! backup accounts can sit idle indefinitely until failover. Every few minutes this task
//! picks one standby whose 5h window is still unused and sends one Codex-shaped Responses
//! turn with that profile's own AuthManager. It does not call `activate` / `lease`, so the
//! active execution identity and prompt cache stay put.
//!
//! A pass lists models through the same `ModelsManager` path as the session
//! picker and `codex debug models` (`GET /models`, cache then network). It then
//! posts one turn with a ChatGPT-capable slug from that catalog, reads 5h
//! usage, and updates rate limits when the window started. Unknown or reserved
//! slugs are never posted. If the API rejects the first slug as unusable, the
//! same pass tries the catalog default once. `/models` is existence/source of
//! truth, not a ChatGPT allowlist — gpt-5.2 still appears there. If the window
//! still did not start, the attempt is logged and otherwise discarded — no
//! Failed observation, no backoff clock. The next interval retries any profile
//! still at 0%.

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
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::SessionSource;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::warn;

use crate::account_window_warmup_request::is_warmup_tool_call;
use crate::account_window_warmup_request::warmup_prompt;
use crate::account_window_warmup_request::warmup_responses_metadata;
use crate::client::ModelClient;
use crate::client::agent_identity_auth_policy;
use crate::client_common::Prompt;
use crate::config::Config;
use crate::resolve_installation_id;
use crate::responses_metadata::CodexResponsesMetadata;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_rollout_trace::InferenceTraceContext;

/// Full Codex instructions + tools take longer than the old toy prompt.
const PER_PROFILE_TIMEOUT: Duration = Duration::from_secs(90);
const GET_REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
/// Cold-idle Responses headers usually still show 0%. Poll accounts usage a few times before
/// declaring NOOP so lagging GETs do not produce false "warmup retry" loops.
const GET_VERIFY_DELAYS_SECS: &[u64] = &[0, 1, 2, 4, 8];
const URGENT_WARMUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Give quota probes a moment to land, then start warming — do not wait a full interval.
const INITIAL_WARMUP_SETTLE: Duration = Duration::from_secs(30);

/// Use the process originator (same header as interactive turns). A made-up warmup
/// originator is not first-party and can be rejected independently of ChatGPT bearer auth.
fn warmup_originator() -> String {
    codex_login::default_client::originator().value
}

/// Spawns the periodic standby-window warmup loop. The caller owns the handle and may abort it.
pub(crate) fn spawn_window_warmup_task(pool: Arc<AccountPool>, config: Config) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Settle install/quota probes before the first pass.
        tokio::time::sleep(INITIAL_WARMUP_SETTLE).await;
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
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let lock_store = store.clone();
    let _lock = tokio::task::spawn_blocking(move || lock_store.lock_window_warmup())
        .await
        .map_err(|error| anyhow::anyhow!("window warmup lock join failed: {error}"))??;
    store.apply_window_warmup_to_pool(pool)?;
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
    let result = warm_profile(pool, config, &profile_id, auth_manager).await;
    if let Err(error) = store.synchronize(pool) {
        debug!(error = %error, "failed to persist window warmup observation");
    }
    result
}

async fn warm_profile(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
) -> anyhow::Result<()> {
    let attempted_at = Utc::now();
    // CLI re-login may have refreshed tokens on disk while this process still holds a stale cache.
    let _ = auth_manager.reload().await;
    let Some(auth) = auth_manager.auth().await.filter(CodexAuth::is_chatgpt_auth) else {
        debug!(%profile_id, "skipping window warmup without ChatGPT auth");
        return Ok(());
    };

    // Keep the session provider as-is (including websockets). A new ModelClient /
    // thread does not share the active session socket. Forcing HTTP was another
    // unusable dependency: interactive Codex turns meter the 5h window and emit
    // `codex.rate_limits` on the session websocket.
    let provider = config.model_provider.clone();

    // Official Codex list for this profile's auth. OnlineIfUncached matches
    // interactive turns. Do not call get_default_model: a configured slug is
    // returned unvalidated. One catalog only — never mix live slugs with
    // bundled names the backend may not have.
    let catalog = crate::thread_manager::build_models_manager(config, Arc::clone(&auth_manager))
        .raw_model_catalog(
            codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    let models = codex_models_manager::select_warmup_models(&catalog, config.model.as_deref());
    if models.is_empty() {
        warn!(%profile_id, "standby window warmup skipped: no catalog-backed ChatGPT model");
        return Ok(());
    }
    let thread_id = ThreadId::new();
    let client = ModelClient::new(
        Some(Arc::clone(&auth_manager)),
        agent_identity_auth_policy(&config.features),
        thread_id,
        provider,
        SessionSource::Cli,
        warmup_originator(),
        /*model_verbosity*/ None,
        /*content_item_kinds_enabled*/ true,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        config.http_client_factory(),
    );
    // Interactive turns persist a UUID installation id and reject non-UUID files.
    // The previous literal is not a UUID and is not a real install identity.
    let installation_id = resolve_installation_id(&config.codex_home)
        .await
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let responses_metadata = warmup_responses_metadata(installation_id, thread_id);

    // Share observed rate limits outside the timeout future so a late timeout can still keep
    // headers that already arrived (timeout otherwise drops them and falsely records failure).
    let observed_limits = Arc::new(tokio::sync::Mutex::new(None));
    let mut stream_limits = None;
    for (index, model_info) in models.iter().enumerate() {
        let effort = codex_models_manager::warmup_supported_effort(
            model_info,
            config.model_reasoning_effort.as_ref(),
        );
        debug!(
            %profile_id,
            model = %model_info.slug,
            ?effort,
            use_responses_lite = model_info.use_responses_lite,
            "starting standby window warmup"
        );
        *observed_limits.lock().await = None;
        let session_telemetry = SessionTelemetry::new(
            thread_id,
            &model_info.slug,
            &model_info.slug,
            /*account_id*/ None,
            /*account_email*/ None,
            /*auth_mode*/ None,
            warmup_originator(),
            /*log_user_prompts*/ false,
            "account-window-warmup".to_string(),
            SessionSource::Cli,
        );
        let prompt = warmup_prompt(model_info, config);
        let stream_result = tokio::time::timeout(
            PER_PROFILE_TIMEOUT,
            stream_warmup_turn(
                &client,
                &prompt,
                model_info,
                &session_telemetry,
                effort,
                &responses_metadata,
                &observed_limits,
            ),
        )
        .await;
        match stream_result {
            Ok(Ok(observed)) => {
                stream_limits = observed;
                break;
            }
            Ok(Err(error)) => {
                let can_retry = index + 1 < models.len()
                    && codex_models_manager::is_unusable_warmup_model_error(&error.to_string());
                if can_retry {
                    warn!(
                        %profile_id,
                        rejected_model = %model_info.slug,
                        fallback_model = %models[index + 1].slug,
                        error = %error,
                        "standby window warmup rejected an unusable model; trying the catalog default"
                    );
                    continue;
                }
                warn!(%profile_id, error = %error, "standby window warmup request failed");
                return Ok(());
            }
            Err(_elapsed) => {
                let partial = observed_limits.lock().await.clone();
                if partial
                    .as_ref()
                    .and_then(|snapshot| snapshot.primary.as_ref())
                    .is_some_and(|window| window.used_percent > 0.0)
                {
                    warn!(
                        %profile_id,
                        "standby window warmup timed out after rate-limit headers; keeping partial success"
                    );
                    stream_limits = partial;
                    break;
                }
                warn!(%profile_id, "standby window warmup timed out");
                return Ok(());
            }
        }
    }

    let stream_started = stream_limits
        .as_ref()
        .and_then(|snapshot| snapshot.primary.as_ref())
        .is_some_and(|window| window.used_percent > 0.0);
    let stream_account_limits = stream_limits.as_ref().map(convert_rate_limits);

    // earliest-reset can activate this profile while the POST is in flight. Pool writes are
    // monotonic, so a 0% GET cannot unstart an active session. Do not skip GET verify or
    // outcome recording — cold-idle Responses headers are usually 0%, and skipping here
    // silently drops both success and NOOP (reporter: "profile became active mid-warmup"
    // after a Luna POST, leftover Failed stays on the now-current account).
    let still_standby = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .is_some_and(|snapshot| !snapshot.is_active);

    let mut started = stream_started;
    let mut best_limits = stream_account_limits.clone();

    if let Some(limits) = stream_account_limits.as_ref() {
        pool.update_rate_limits(profile_id, limits.clone())?;
        if still_standby {
            debug!(%profile_id, "warmed standby 5h rate-limit window");
        } else {
            debug!(%profile_id, "kept mid-warmup stream evidence after profile activated");
        }
    } else {
        debug!(%profile_id, "warmup completed without rate-limit headers");
    }

    // Cold-idle Responses headers typically still show 0%. Retry accounts usage GET with short
    // delays before declaring NOOP — lagging GETs were the main false "warmup retry" source.
    if let Some(limits) =
        refresh_rate_limits_via_get_with_retries(config, &auth, stream_started).await
    {
        if account_primary_started(&limits) {
            started = true;
        }
        let merged = merge_account_rate_limits_monotonic(best_limits.as_ref(), limits);
        pool.update_rate_limits(profile_id, merged.clone())?;
        best_limits = Some(merged);
        debug!(%profile_id, "refreshed standby rate limits after window warmup");
    }

    // Prefer local evidence over a racy pool re-read: concurrent quota sync can briefly regress
    // primary usage back to 0% after we already observed a start.
    if !started && !primary_window_started(pool, profile_id) {
        warn!(
            %profile_id,
            stream_started,
            get_primary = best_limits
                .as_ref()
                .and_then(|limits| limits.primary.as_ref())
                .map(|window| window.used_percent),
            "standby window warmup completed without starting the 5h window"
        );
        return Ok(());
    }

    if let Some(limits) = best_limits.as_ref()
        && account_primary_started(limits)
    {
        let _ = pool.update_rate_limits(profile_id, limits.clone());
    }

    let _ = pool.record_window_warmup(
        profile_id,
        WindowWarmupObservation::current(
            WindowWarmupOutcome::Succeeded,
            attempted_at,
            None,
            /*consecutive_failures*/ 0,
        ),
    );
    Ok(())
}

async fn stream_warmup_turn(
    client: &ModelClient,
    prompt: &Prompt,
    model_info: &ModelInfo,
    session_telemetry: &SessionTelemetry,
    effort: Option<ReasoningEffort>,
    responses_metadata: &CodexResponsesMetadata,
    observed_limits: &Arc<tokio::sync::Mutex<Option<RateLimitSnapshot>>>,
) -> anyhow::Result<Option<RateLimitSnapshot>> {
    let mut session = client.new_session();
    let mut stream = session
        .stream(
            prompt,
            model_info,
            session_telemetry,
            effort,
            ReasoningSummary::None,
            /*service_tier*/ None,
            responses_metadata,
            &InferenceTraceContext::disabled(),
        )
        .await?;
    let mut observed = None;
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::RateLimits(snapshot) => {
                let preferred = prefer_rate_limit_snapshot(observed.clone(), snapshot);
                observed = Some(preferred.clone());
                *observed_limits.lock().await = Some(preferred);
            }
            ResponseEvent::Completed { .. } => break,
            ResponseEvent::OutputItemDone(item) | ResponseEvent::OutputItemAdded(item)
                if is_warmup_tool_call(&item) =>
            {
                break;
            }
            _ => {}
        }
    }
    Ok(observed)
}

fn prefer_rate_limit_snapshot(
    current: Option<RateLimitSnapshot>,
    incoming: RateLimitSnapshot,
) -> RateLimitSnapshot {
    let Some(current) = current else {
        return incoming;
    };
    let incoming_is_codex = incoming
        .limit_id
        .as_deref()
        .is_none_or(|limit_id| limit_id == "codex");
    let current_is_codex = current
        .limit_id
        .as_deref()
        .is_none_or(|limit_id| limit_id == "codex");
    match (current_is_codex, incoming_is_codex) {
        (false, true) => incoming,
        (true, false) => current,
        _ => {
            let current_used = current
                .primary
                .as_ref()
                .map(|window| window.used_percent)
                .unwrap_or(0.0);
            let incoming_used = incoming
                .primary
                .as_ref()
                .map(|window| window.used_percent)
                .unwrap_or(0.0);
            if incoming_used > current_used {
                incoming
            } else {
                current
            }
        }
    }
}

fn account_primary_started(limits: &AccountRateLimits) -> bool {
    limits
        .primary
        .as_ref()
        .is_some_and(|window| window.used_percent > 0.0)
}

fn merge_account_rate_limits_monotonic(
    existing: Option<&AccountRateLimits>,
    incoming: AccountRateLimits,
) -> AccountRateLimits {
    let Some(existing) = existing else {
        return incoming;
    };
    let mut merged = incoming;
    if let Some(existing_primary) = existing.primary.as_ref()
        && existing_primary.used_percent > 0.0
    {
        let regresses = merged
            .primary
            .as_ref()
            .is_none_or(|window| window.used_percent <= 0.0);
        let reset_due = existing_primary
            .resets_at
            .is_some_and(|resets_at| resets_at <= Utc::now());
        if regresses && !reset_due {
            merged.primary = Some(existing_primary.clone());
        }
    }
    if merged.observed_at.is_none() {
        merged.observed_at = existing.observed_at;
    }
    merged
}

async fn refresh_rate_limits_via_get_with_retries(
    config: &Config,
    auth: &CodexAuth,
    stream_started: bool,
) -> Option<AccountRateLimits> {
    let mut best = None;
    for (index, delay_secs) in GET_VERIFY_DELAYS_SECS.iter().enumerate() {
        if *delay_secs > 0 {
            tokio::time::sleep(Duration::from_secs(*delay_secs)).await;
        }
        let Some(limits) = refresh_rate_limits_via_get(config, auth).await else {
            continue;
        };
        if account_primary_started(&limits) {
            return Some(limits);
        }
        // Keep the freshest idle snapshot; if Responses already proved a start, preserve that
        // below via monotonic merge with stream limits.
        best = Some(limits);
        // After a stream-proven start, one confirming GET is enough.
        if stream_started && index == 0 {
            break;
        }
    }
    best
}

fn primary_window_started(pool: &AccountPool, profile_id: &AccountProfileId) -> bool {
    pool.snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.rate_limits.primary)
        .is_some_and(|window| window.used_percent > 0.0)
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
        window_minutes: window.window_minutes,
    }
}

#[cfg(test)]
#[path = "account_window_warmup_tests.rs"]
mod tests;
