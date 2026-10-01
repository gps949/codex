//! Identity-preserving request execution for standby window warmup.

use super::*;

pub(super) async fn warm_profile(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
) -> anyhow::Result<WarmupAttemptOutcome> {
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return Ok(WarmupAttemptOutcome::Failed);
    }
    let attempted_at = Utc::now();
    // CLI re-login may have refreshed tokens on disk while this process still holds a stale cache.
    let _ = auth_manager.reload().await;
    let Some(auth) = auth_manager.auth().await.filter(CodexAuth::is_chatgpt_auth) else {
        debug!(%profile_id, "skipping window warmup without ChatGPT auth");
        record_window_warmup_debug(WindowWarmupDebugKind::SkipNoAuth {
            profile_id: profile_id.to_string(),
        });
        return Ok(WarmupAttemptOutcome::SkippedNoAuth);
    };

    let needs_refresh = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.rate_limits.primary)
        .is_none_or(|window| window.resets_at.is_some_and(|reset| reset <= attempted_at));
    if needs_refresh && let Some(limits) = refresh_rate_limits_via_get(config, &auth).await {
        let started = account_primary_started(&limits);
        pool.update_rate_limits(profile_id, limits)?;
        if started {
            pool.record_window_warmup(
                profile_id,
                WindowWarmupObservation::current(
                    WindowWarmupOutcome::Succeeded,
                    attempted_at,
                    /*retry_after*/ None,
                    /*consecutive_failures*/ 0,
                ),
            )?;
            return Ok(WarmupAttemptOutcome::Started);
        }
    }

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
        record_window_warmup_debug(WindowWarmupDebugKind::CatalogEmpty {
            profile_id: profile_id.to_string(),
        });
        return Ok(WarmupAttemptOutcome::Failed);
    }
    record_window_warmup_debug(WindowWarmupDebugKind::CatalogResolved {
        profile_id: profile_id.to_string(),
        slugs: models.iter().map(|model| model.slug.clone()).collect(),
    });
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
        config
            .features
            .enabled(codex_features::Feature::ReasoningEffortOverride),
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        config.http_client_factory(),
        config.workspace_routing_context(),
        Vec::new(),
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
    let mut request_completed = false;
    for (index, model_info) in models.iter().enumerate() {
        if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
            return Ok(WarmupAttemptOutcome::Failed);
        }
        let effort = codex_models_manager::warmup_supported_effort(model_info);
        debug!(
            %profile_id,
            model = %model_info.slug,
            ?effort,
            use_responses_lite = model_info.use_responses_lite,
            "starting standby window warmup"
        );
        record_window_warmup_debug(WindowWarmupDebugKind::RequestStart {
            profile_id: profile_id.to_string(),
            model: model_info.slug.clone(),
            effort: effort
                .as_ref()
                .map(|value| format!("{value:?}"))
                .unwrap_or_else(|| "none".to_string()),
            use_responses_lite: model_info.use_responses_lite,
        });
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
        let prompt = warmup_prompt(model_info);
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
                request_completed = true;
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
                    record_window_warmup_debug(WindowWarmupDebugKind::ModelRejected {
                        profile_id: profile_id.to_string(),
                        rejected_model: model_info.slug.clone(),
                        fallback_model: models[index + 1].slug.clone(),
                        error: error.to_string(),
                    });
                    continue;
                }
                warn!(%profile_id, error = %error, "standby window warmup request failed");
                record_window_warmup_debug(WindowWarmupDebugKind::RequestFailed {
                    profile_id: profile_id.to_string(),
                    error: error.to_string(),
                });
                stream_limits = observed_limits.lock().await.clone();
                // The POST already went out. Keep GET-verify — a tool-call
                // stream can end without Completed and still start the 5h window.
                break;
            }
            Err(_elapsed) => {
                stream_limits = observed_limits.lock().await.clone();
                warn!(%profile_id, "standby window warmup timed out");
                record_window_warmup_debug(WindowWarmupDebugKind::RequestTimeout {
                    profile_id: profile_id.to_string(),
                });
                // Usage can be published after the stream times out. Keep verifying before
                // treating this as a failure or issuing another generating request.
                break;
            }
        }
    }

    let stream_account_limits = stream_limits.as_ref().map(convert_rate_limits);
    let stream_started = stream_account_limits
        .as_ref()
        .is_some_and(account_primary_started);

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
        record_window_warmup_debug(WindowWarmupDebugKind::Noop {
            profile_id: profile_id.to_string(),
            stream_started,
            get_primary: best_limits
                .as_ref()
                .and_then(|limits| limits.primary.as_ref())
                .map(|window| window.used_percent.to_string()),
        });
        return Ok(if request_completed {
            WarmupAttemptOutcome::Unconfirmed
        } else {
            WarmupAttemptOutcome::Failed
        });
    }

    if let Some(limits) = best_limits.as_ref()
        && account_primary_started(limits)
    {
        let _ = pool.update_rate_limits(profile_id, limits.clone());
    }

    let used_percent = best_limits
        .as_ref()
        .and_then(|limits| limits.primary.as_ref())
        .map(|window| window.used_percent.to_string())
        .unwrap_or_else(|| "started".to_string());
    let _ = pool.record_window_warmup(
        profile_id,
        WindowWarmupObservation::current(
            WindowWarmupOutcome::Succeeded,
            attempted_at,
            None,
            /*consecutive_failures*/ 0,
        ),
    );
    record_window_warmup_debug(WindowWarmupDebugKind::Succeeded {
        profile_id: profile_id.to_string(),
        used_percent,
    });
    Ok(WarmupAttemptOutcome::Started)
}
