//! Identity-preserving request execution for standby window warmup.

use super::quota::*;
use super::*;
use codex_login::AccountAvailability;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_protocol::config_types::Verbosity;
use std::sync::atomic::AtomicBool;

#[cfg(test)]
pub(super) async fn warm_profile(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
) -> anyhow::Result<WarmupAttemptOutcome> {
    let guard = Arc::new(super::guard::WarmupRequestGuard::new(
        AccountRuntimeStateStore::new(config.codex_home.to_path_buf()),
        profile_id.clone(),
        Utc::now(),
    ));
    warm_profile_with_guard(pool, config, profile_id, auth_manager, guard).await
}

pub(super) async fn warm_profile_with_guard(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
    request_guard: Arc<super::guard::WarmupRequestGuard>,
) -> anyhow::Result<WarmupAttemptOutcome> {
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return Ok(WarmupAttemptOutcome::Failed);
    }
    let attempted_at = request_guard.attempted_at();
    // CLI re-login may have refreshed tokens on disk while this process still holds a stale cache.
    let expected_auth = auth_manager.auth_cached();
    auth_manager.reload().await;
    let Some((auth, mut factory)) = auth_manager
        .auth_with_http_client_factory()
        .await
        .filter(|(auth, _)| auth.is_chatgpt_auth())
    else {
        debug!(%profile_id, "skipping window warmup without ChatGPT auth");
        record_window_warmup_debug(WindowWarmupDebugKind::SkipNoAuth {
            profile_id: profile_id.to_string(),
        });
        return Ok(WarmupAttemptOutcome::SkippedNoAuth);
    };
    if expected_auth
        .as_ref()
        .is_some_and(|expected| !same_warmup_identity(expected, &auth))
    {
        return Ok(WarmupAttemptOutcome::SkippedNoAuth);
    }
    let mut profile_config = config.clone();
    if let Some(clients) = auth_manager.maintenance_clients(&auth).await? {
        factory = clients.http_client_factory;
        profile_config.chatgpt_base_url = clients.chatgpt_base_url;
    }
    profile_config.application_network_policy = factory.network_policy().clone();
    profile_config.application_auth_route_config = Some(
        codex_login::AuthRouteConfig::from_http_client_factory(factory.clone()),
    );
    let config = &profile_config;
    // Even a cached idle account can have been used by another Codex process or device. Verify
    // every generating attempt instead of spending quota based on a stale 0% observation.
    if let Some(probe) = refresh_rate_limits_via_get(config, &auth_manager, &auth).await? {
        let allowed = probe.ordinary_allowed && quota_allows_generation(&probe.limits);
        let limits = probe.limits;
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
        if !allowed {
            record_window_warmup_debug(WindowWarmupDebugKind::RequestFailed {
                profile_id: profile_id.to_string(),
                error: "quota preflight found an exhausted or unsupported window; no generating request sent".to_string(),
            });
            return Ok(WarmupAttemptOutcome::SkippedNotEligible);
        }
    } else {
        record_window_warmup_debug(WindowWarmupDebugKind::RequestFailed {
            profile_id: profile_id.to_string(),
            error: "quota preflight unavailable; no generating request sent".to_string(),
        });
        return Ok(WarmupAttemptOutcome::Failed);
    }

    // Keep the session provider as-is (including websockets). A new ModelClient /
    // thread does not share the active session socket. Forcing HTTP was another
    // unusable dependency: interactive Codex turns meter the 5h window and emit
    // `codex.rate_limits` on the session websocket.
    let mut provider = config.model_provider.clone();
    // Transport retries cannot establish whether an interrupted maintenance POST was billed.
    // Leave auth recovery to ModelClient, but never repeat an ambiguous generating request.
    provider.request_max_retries = Some(0);
    provider.stream_max_retries = Some(0);

    // Official Codex list for this profile's auth. OnlineIfUncached matches
    // interactive turns. Do not call get_default_model: a configured slug is
    // returned unvalidated. One catalog only — never mix live slugs with
    // bundled names the backend may not have.
    let catalog = crate::thread_manager::build_models_manager(config, Arc::clone(&auth_manager))
        .raw_model_catalog(
            codex_models_manager::manager::RefreshStrategy::OnlineIfUncached,
            factory.clone(),
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
        models
            .iter()
            .all(|model| model.support_verbosity)
            .then_some(Verbosity::Low),
        /*content_item_kinds_enabled*/ true,
        config
            .features
            .enabled(codex_features::Feature::ReasoningEffortOverride),
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*concurrent_reasoning_summaries_enabled*/ false,
        /*attestation_provider*/ None,
        factory,
        config.workspace_routing_context(),
        Vec::new(),
    )
    .with_captured_chatgpt_identity(&auth)?
    .with_warmup_request_guard(Arc::clone(&request_guard));
    // Interactive turns persist a UUID installation id and reject non-UUID files.
    // The previous literal is not a UUID and is not a real install identity.
    let installation_id = resolve_installation_id(&config.codex_home)
        .await
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let responses_metadata = warmup_responses_metadata(installation_id, thread_id);

    // Share observed rate limits outside the timeout future so a late timeout can still keep
    // headers that already arrived (timeout otherwise drops them and falsely records failure).
    let observed_limits = Arc::new(tokio::sync::Mutex::new(None));
    let accepted = Arc::new(AtomicBool::new(false));
    let mut stream_limits = None;
    let mut request_maybe_accepted = false;
    for (index, model_info) in models.iter().enumerate() {
        if !profile_allows_generation(pool, config, profile_id)? {
            return Ok(WarmupAttemptOutcome::SkippedNotEligible);
        }
        let Some((current_auth, _)) = auth_manager.auth_with_http_client_factory().await else {
            return Ok(WarmupAttemptOutcome::SkippedNoAuth);
        };
        if !same_warmup_identity(&auth, &current_auth) {
            return Ok(WarmupAttemptOutcome::SkippedNoAuth);
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
        accepted.store(false, Ordering::Relaxed);
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
                WarmupStreamEvidence {
                    observed_limits: &observed_limits,
                    accepted: &accepted,
                },
            ),
        )
        .await;
        match stream_result {
            Ok(Ok(observed)) => {
                request_maybe_accepted = true;
                stream_limits = observed;
                break;
            }
            Ok(Err(error)) => {
                let definitely_rejected =
                    !accepted.load(Ordering::Relaxed) && request_was_definitely_rejected(&error);
                let can_retry = definitely_rejected
                    && index + 1 < models.len()
                    && codex_models_manager::is_unusable_warmup_model_error(&error.to_string());
                if definitely_rejected {
                    request_guard.definite_rejection();
                }
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
                request_maybe_accepted = !definitely_rejected && request_guard.may_have_been_sent();
                // The POST already went out. Keep GET-verify — a tool-call
                // stream can end without Completed and still start the 5h window.
                break;
            }
            Err(_elapsed) => {
                stream_limits = observed_limits.lock().await.clone();
                request_maybe_accepted = request_guard.may_have_been_sent();
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

    auth_manager.reload().await;
    if !auth_manager
        .auth_cached()
        .as_ref()
        .is_some_and(|current| same_warmup_identity(&auth, current))
    {
        return Ok(if request_maybe_accepted {
            WarmupAttemptOutcome::Unconfirmed
        } else {
            WarmupAttemptOutcome::SkippedNoAuth
        });
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
        if pool.update_rate_limits(profile_id, limits.clone()).is_err() {
            return Ok(WarmupAttemptOutcome::Unconfirmed);
        }
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
    let verified =
        refresh_rate_limits_via_get_with_retries(config, &auth_manager, &auth, stream_started)
            .await;
    if verified.is_err() && request_maybe_accepted {
        return Ok(WarmupAttemptOutcome::Unconfirmed);
    }
    if let Some(limits) = verified? {
        if account_primary_started(&limits) {
            started = true;
        }
        let merged = merge_account_rate_limits_monotonic(best_limits.as_ref(), limits);
        if pool.update_rate_limits(profile_id, merged.clone()).is_err() {
            return Ok(WarmupAttemptOutcome::Unconfirmed);
        }
        best_limits = Some(merged);
        debug!(%profile_id, "refreshed standby rate limits after window warmup");
    }

    // Prefer local evidence over a racy pool re-read: concurrent quota sync can briefly regress
    // primary usage back to 0% after we already observed a start.
    if !started {
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
        return Ok(if request_maybe_accepted {
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

pub(super) async fn confirm_profile(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
    auth_manager: Arc<AuthManager>,
) -> anyhow::Result<()> {
    let Some(expected_auth) = auth_manager
        .auth_cached()
        .filter(CodexAuth::is_chatgpt_auth)
    else {
        return Ok(());
    };
    let mut profile_config = config.clone();
    if let Some(clients) = auth_manager.maintenance_clients(&expected_auth).await? {
        profile_config.chatgpt_base_url = clients.chatgpt_base_url;
        profile_config.application_network_policy =
            clients.http_client_factory.network_policy().clone();
        profile_config.application_auth_route_config = Some(
            codex_login::AuthRouteConfig::from_http_client_factory(clients.http_client_factory),
        );
    }
    let Some(probe) =
        refresh_rate_limits_via_get(&profile_config, &auth_manager, &expected_auth).await?
    else {
        return Ok(());
    };
    let limits = probe.limits;
    let started = account_primary_started(&limits);
    let observation = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.window_warmup);
    pool.update_rate_limits(profile_id, limits)?;
    if started && let Some(observation) = observation {
        pool.record_window_warmup(
            profile_id,
            WindowWarmupObservation::current(
                WindowWarmupOutcome::Succeeded,
                observation.attempted_at,
                /*retry_after*/ None,
                /*consecutive_failures*/ 0,
            ),
        )?;
    }
    Ok(())
}

pub(super) fn profile_allows_generation(
    pool: &AccountPool,
    config: &Config,
    profile_id: &AccountProfileId,
) -> anyhow::Result<bool> {
    if !config.account_pool.effective_window_warmup()
        || codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home)
    {
        return Ok(false);
    }
    let Some(snapshot) = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| &snapshot.profile.id == profile_id)
    else {
        return Ok(false);
    };
    let available = match snapshot.availability {
        AccountAvailability::Available => true,
        AccountAvailability::Exhausted {
            resets_at: Some(reset),
        } => reset <= Utc::now(),
        AccountAvailability::Exhausted { resets_at: None }
        | AccountAvailability::AuthenticationUnavailable { .. }
        | AccountAvailability::Disabled => false,
    };
    if !available || snapshot.profile.disabled || snapshot.is_active {
        return Ok(false);
    }
    let now = Utc::now();
    let quota_fresh = snapshot
        .rate_limits
        .observed_at
        .is_some_and(|observed| observed <= now && now - observed < chrono::Duration::minutes(30));
    if account_primary_started(&snapshot.rate_limits)
        || quota_fresh
            && snapshot
                .rate_limits
                .primary
                .iter()
                .chain(snapshot.rate_limits.secondary.iter())
                .any(|window| {
                    window.used_percent >= 100.0 && window.resets_at.is_none_or(|reset| reset > now)
                })
    {
        return Ok(false);
    }
    let profiles = AccountProfileStore::new(config.codex_home.to_path_buf());
    if profiles.manifest_path().exists()
        && !profiles.load_profile_records()?.iter().any(|record| {
            record.profile.id == *profile_id
                && record.profile.credential_home == snapshot.profile.credential_home
                && !record.profile.disabled
                && record.state == AccountProfileState::Ready
        })
    {
        return Ok(false);
    }
    Ok(
        AccountRuntimeStateStore::new(config.codex_home.to_path_buf())
            .load()?
            .active_profile_id
            .as_ref()
            != Some(profile_id),
    )
}
