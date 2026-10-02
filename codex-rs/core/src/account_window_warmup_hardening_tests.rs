//! Request/confirmation regressions for interrupted, stale, and remotely modified warmup.

use super::*;
use pretty_assertions::assert_eq;

fn usage_body(primary: u32, secondary: Option<u32>, primary_seconds: u32) -> serde_json::Value {
    serde_json::json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": primary,
                "limit_window_seconds": primary_seconds,
                "reset_after_seconds": primary_seconds,
                "reset_at": Utc::now().timestamp() + i64::from(primary_seconds),
            },
            "secondary_window": secondary.map(|used| serde_json::json!({
                "used_percent": used,
                "limit_window_seconds": 604800,
                "reset_after_seconds": 200000,
                "reset_at": Utc::now().timestamp() + 200000,
            })),
        }
    })
}

async fn generating_requests(fixture: &WarmupRequestFixture) -> usize {
    fixture
        .server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .count()
}

#[tokio::test]
async fn warmup_preflight_suppresses_depleted_weekly_and_unsupported_primary() -> anyhow::Result<()>
{
    for (weekly, primary_seconds) in [(Some(100), 18000), (None, 604800)] {
        let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(
                ResponseTemplate::new(/*status*/ 200).set_body_json(usage_body(
                    0,
                    weekly,
                    primary_seconds,
                )),
            )
            .mount(&fixture.server)
            .await;
        let outcome = warm_profile(
            &fixture.pool,
            &fixture.config,
            &fixture.profile_id,
            Arc::clone(&fixture.auth_manager),
        )
        .await?;
        assert!(matches!(outcome, WarmupAttemptOutcome::SkippedNotEligible));
        assert_eq!(generating_requests(&fixture).await, 0);
    }
    Ok(())
}

#[tokio::test]
async fn warmup_missing_preflight_does_not_blindly_generate() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(/*status*/ 503))
        .mount(&fixture.server)
        .await;
    let outcome = warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        Arc::clone(&fixture.auth_manager),
    )
    .await?;
    assert!(matches!(outcome, WarmupAttemptOutcome::Failed));
    assert_eq!(generating_requests(&fixture).await, 0);
    Ok(())
}

#[tokio::test]
async fn interrupted_warmup_is_only_confirmed_without_another_generating_request()
-> anyhow::Result<()> {
    let fixture = warmup_request_fixture_with_sse(
        /*enable_agent_identity*/ false,
        /*primary_used_percent*/ None,
        sse(vec![ev_response_created("resp-interrupted")]),
        /*first_unusable_model_message*/ None,
    )
    .await?;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    let observation = fixture.pool.snapshots()[0]
        .window_warmup
        .clone()
        .expect("interrupted observation");
    assert_eq!(
        observation,
        WindowWarmupObservation::unconfirmed(observation.attempted_at)
    );
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200).set_body_json(usage_body(1, None, 18000)),
        )
        .mount(&fixture.server)
        .await;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    assert_eq!(generating_requests(&fixture).await, 1);
    assert_eq!(
        warmup_outcome(&fixture.pool, &fixture.profile_id),
        WindowWarmupOutcome::Succeeded
    );
    Ok(())
}

#[tokio::test]
async fn warmup_pass_skips_busy_lock_and_disabled_config() -> anyhow::Result<()> {
    let mut fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let lock = store.try_lock_window_warmup()?.expect("first owner");
    tokio::time::timeout(
        Duration::from_secs(1),
        run_warmup_pass(&fixture.pool, &fixture.config),
    )
    .await??;
    assert_eq!(warmup_outcome_opt(&fixture.pool, &fixture.profile_id), None);
    drop(lock);
    fixture.config.account_pool.window_warmup = Some(false);
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    assert_eq!(warmup_outcome_opt(&fixture.pool, &fixture.profile_id), None);
    assert_eq!(generating_requests(&fixture).await, 0);
    Ok(())
}

#[tokio::test]
async fn warmup_respects_profile_disable_after_catalog_lookup() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let profile = fixture.pool.snapshots()[0].profile.clone();
    let profiles = codex_login::AccountProfileStore::new(fixture.config.codex_home.to_path_buf());
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(".*/models$"))
        .respond_with(move |_request: &wiremock::Request| {
            profiles
                .update_profile_metadata(
                    &profile.id,
                    codex_login::AccountProfileMetadataUpdate {
                        disabled: Some(true),
                        ..Default::default()
                    },
                )
                .expect("disable target during catalog lookup");
            ResponseTemplate::new(/*status*/ 200).set_body_json(live_warmup_catalog("gpt-6-astra"))
        })
        .mount(&fixture.server)
        .await;
    let outcome = warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        Arc::clone(&fixture.auth_manager),
    )
    .await?;
    assert!(matches!(outcome, WarmupAttemptOutcome::SkippedNotEligible));
    assert_eq!(generating_requests(&fixture).await, 0);
    Ok(())
}

#[tokio::test]
async fn relogin_during_quota_probe_does_not_publish_another_seat() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let auth_path = fixture.config.codex_home.join("auth.json");
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |_request: &wiremock::Request| {
            let mut auth: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&auth_path).expect("auth"))
                    .expect("auth JSON");
            auth["tokens"]["account_id"] = serde_json::json!("different-workspace");
            std::fs::write(&auth_path, serde_json::to_string(&auth).expect("auth JSON"))
                .expect("relogin");
            ResponseTemplate::new(/*status*/ 200).set_body_json(usage_body(1, None, 18000))
        })
        .mount(&fixture.server)
        .await;
    let outcome = warm_profile(
        &fixture.pool,
        &fixture.config,
        &fixture.profile_id,
        Arc::clone(&fixture.auth_manager),
    )
    .await;
    assert!(outcome.is_err());
    assert_eq!(
        fixture.pool.snapshots()[0]
            .rate_limits
            .primary
            .as_ref()
            .map(|window| window.used_percent),
        Some(0.0)
    );
    assert_eq!(generating_requests(&fixture).await, 0);
    Ok(())
}

#[test]
fn stream_merge_preserves_both_windows_and_accepts_new_reset_epoch() {
    let now = Utc::now().timestamp();
    let mut previous = rate_limit_snapshot(Some("codex"), 90.0);
    previous.primary.as_mut().expect("primary").resets_at = Some(now + 60);
    previous.secondary = Some(RateLimitWindow {
        used_percent: 40.0,
        resets_at: Some(now + 200000),
        window_minutes: Some(10080),
    });
    let mut next = rate_limit_snapshot(Some("codex"), 0.0);
    next.primary.as_mut().expect("primary").resets_at = Some(now + 18000);
    let merged = prefer_rate_limit_snapshot(Some(previous.clone()), next.clone());
    next.secondary = previous.secondary;
    assert_eq!(merged, next);

    let mut partial = rate_limit_snapshot(Some("codex"), 1.0);
    partial.primary = None;
    partial.secondary = Some(RateLimitWindow {
        used_percent: 42.0,
        resets_at: Some(now + 200000),
        window_minutes: Some(10080),
    });
    let merged = prefer_rate_limit_snapshot(Some(next.clone()), partial.clone());
    partial.primary = next.primary;
    assert_eq!(merged, partial);
}

#[test]
fn warmup_only_retries_definite_request_rejections() {
    let invalid = anyhow::Error::from(codex_protocol::error::CodexErr::InvalidRequest(
        "unknown model".to_string(),
    ));
    let disconnected = anyhow::Error::from(codex_protocol::error::CodexErr::Stream(
        "connection lost".to_string(),
    ));
    assert!(request_was_definitely_rejected(&invalid));
    assert!(!request_was_definitely_rejected(&disconnected));
}

#[test]
fn primary_usage_without_reset_must_be_recent_to_skip_preflight() {
    let mut limits = AccountRateLimits {
        primary: Some(AccountRateLimitWindow {
            used_percent: 2.0,
            resets_at: None,
            window_minutes: Some(300),
        }),
        secondary: None,
        observed_at: Some(Utc::now() - chrono::Duration::hours(6)),

        window_observed_at: None,
    };
    assert!(!account_primary_started(&limits));
    limits.observed_at = Some(Utc::now());
    assert!(account_primary_started(&limits));
}

#[tokio::test]
async fn warmup_preflight_rejects_backend_denial_and_wrong_seat_without_generation()
-> anyhow::Result<()> {
    for (allowed, account_id, user_id, blocked) in [
        (false, None, None, false),
        (true, Some("different-workspace"), None, false),
        (true, None, Some("different-user"), false),
        (true, None, None, true),
    ] {
        let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
        let mut usage = usage_body(0, None, 18000);
        usage["rate_limit"]["allowed"] = serde_json::json!(allowed);
        if let Some(id) = account_id {
            usage["account_id"] = serde_json::json!(id);
        }
        if let Some(id) = user_id {
            usage["user_id"] = serde_json::json!(id);
        }
        if blocked {
            usage["spend_control"] = serde_json::json!({"reached":true});
        }
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(usage))
            .mount(&fixture.server)
            .await;
        let outcome = warm_profile(
            &fixture.pool,
            &fixture.config,
            &fixture.profile_id,
            Arc::clone(&fixture.auth_manager),
        )
        .await;
        assert!(
            !matches!(outcome, Ok(WarmupAttemptOutcome::Started)),
            "{outcome:?}"
        );
        assert_eq!(generating_requests(&fixture).await, 0);
        assert_eq!(warmup_outcome_opt(&fixture.pool, &fixture.profile_id), None);
    }
    Ok(())
}

#[test]
fn fresh_positive_start_survives_tentative_idle_reset_rounding() {
    let now = Utc::now().timestamp();
    let mut idle = rate_limit_snapshot(Some("codex"), 0.0);
    idle.primary.as_mut().expect("primary").resets_at = Some(now + 18000);
    let mut confirmed = rate_limit_snapshot(Some("codex"), 1.0);
    confirmed.primary.as_mut().expect("primary").resets_at = Some(now + 17999);
    assert_eq!(
        prefer_rate_limit_snapshot(Some(idle), confirmed.clone()),
        confirmed
    );
}

#[tokio::test]
async fn warmup_turning_off_during_quota_preflight_leaves_no_generating_protection()
-> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let home = fixture.config.codex_home.clone();
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |_: &wiremock::Request| {
            codex_login::AccountPoolRuntime::suspend_home(&home)
                .expect("suspend before generation");
            ResponseTemplate::new(/*status*/ 200).set_body_json(usage_body(0, None, 18000))
        })
        .mount(&fixture.server)
        .await;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    assert_eq!(generating_requests(&fixture).await, 0);
    assert!(
        fixture.pool.snapshots()[0]
            .window_warmup
            .as_ref()
            .is_none_or(|state| state.phase.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn warmup_preflight_does_not_generate_from_ambiguous_window_evidence() -> anyhow::Result<()> {
    for (used, window_seconds, reset_seconds) in
        [(1, 18000, 86400), (0, 18000, 86400), (0, 0, 18000)]
    {
        let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
        let mut usage = usage_body(used, None, window_seconds);
        usage["rate_limit"]["primary_window"]["reset_at"] =
            serde_json::json!(Utc::now().timestamp() + reset_seconds);
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(usage))
            .mount(&fixture.server)
            .await;
        let outcome = warm_profile(
            &fixture.pool,
            &fixture.config,
            &fixture.profile_id,
            Arc::clone(&fixture.auth_manager),
        )
        .await?;
        assert!(matches!(outcome, WarmupAttemptOutcome::SkippedNotEligible));
        assert_eq!(generating_requests(&fixture).await, 0);
    }
    Ok(())
}

#[tokio::test]
async fn warmup_send_claim_rejects_a_reset_during_catalog_lookup() -> anyhow::Result<()> {
    let fixture = warmup_request_fixture(/*enable_agent_identity*/ false).await?;
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let profile_id = fixture.profile_id.clone();
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(".*/models$"))
        .respond_with(move |_request: &wiremock::Request| {
            store
                .record_quota_reset(&profile_id, Utc::now())
                .expect("reset during catalog lookup");
            ResponseTemplate::new(/*status*/ 200).set_body_json(live_warmup_catalog("gpt-6-astra"))
        })
        .mount(&fixture.server)
        .await;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    assert_eq!(generating_requests(&fixture).await, 0);
    Ok(())
}

#[tokio::test]
async fn warmup_send_claim_replaces_future_attempt_and_prevents_repeat_generation()
-> anyhow::Result<()> {
    let fixture = warmup_request_fixture_idle_stream(/*enable_agent_identity*/ false).await?;
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let future = WindowWarmupObservation::in_progress(Utc::now() + chrono::Duration::hours(24));
    store.record_window_warmup(&fixture.profile_id, future.clone())?;
    fixture
        .pool
        .record_window_warmup(&fixture.profile_id, future)?;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    run_warmup_pass(&fixture.pool, &fixture.config).await?;
    let state = store.load()?;
    let observation = state
        .profiles
        .iter()
        .find(|profile| profile.profile_id == fixture.profile_id)
        .and_then(|profile| profile.window_warmup.as_ref())
        .expect("durable attempt");
    assert_eq!(
        observation.phase,
        Some(codex_login::WindowWarmupPhase::Unconfirmed)
    );
    assert!(observation.attempted_at <= Utc::now());
    assert_eq!(generating_requests(&fixture).await, 1);
    Ok(())
}
