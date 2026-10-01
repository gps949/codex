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
//! posts one turn with a ChatGPT-capable slug from that catalog, using the
//! same tool harness a session would advertise (`exec`/`wait` for
//! `code_mode_only` models). The stream is drained until `Completed` — aborting
//! on the first tool call cancels billing. Unknown or reserved slugs are never
//! posted. If the API rejects the first slug as unusable, the same pass tries
//! the catalog default once. `/models` is existence/source of truth, not a
//! ChatGPT allowlist — gpt-5.2 still appears there. Every attempt is shared with
//! other processes so unsuccessful standbys do not starve later candidates or
//! consume quota repeatedly while the backend is still publishing usage.

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use codex_backend_client::Client as BackendClient;
use codex_login::AccountPool;
use codex_login::AccountProfileId;
use codex_login::AccountRateLimitWindow;
use codex_login::AccountRateLimits;
use codex_login::AccountRuntimeStateStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::WindowWarmupDebugKind;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
use codex_login::record_window_warmup_debug;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::SessionSource;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::warn;

use crate::account_window_warmup_request::stream_warmup_turn;
use crate::account_window_warmup_request::warmup_prompt;
use crate::account_window_warmup_request::warmup_responses_metadata;
use crate::client::ModelClient;
use crate::client::agent_identity_auth_policy;
use crate::config::Config;
use crate::resolve_installation_id;

#[path = "account_window_warmup_guard.rs"]
pub(crate) mod guard;

#[path = "account_window_warmup_execution.rs"]
mod execution;
#[path = "account_window_warmup_quota.rs"]
pub(crate) mod quota;
use execution::warm_profile;
use quota::*;

/// Full Codex instructions + tools take longer than the old toy prompt.
const PER_PROFILE_TIMEOUT: Duration = Duration::from_secs(90);
const GET_REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
/// Cold-idle Responses headers usually still show 0%. Poll accounts usage a few times before
/// declaring NOOP so lagging GETs do not produce false "warmup retry" loops.
const GET_VERIFY_DELAYS_SECS: &[u64] = &[0, 1, 2, 4, 8];
const URGENT_WARMUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Give quota probes a moment to land, then start warming — do not wait a full interval.
pub(crate) const INITIAL_WARMUP_SETTLE: Duration = Duration::from_secs(30);

/// Use the process originator (same header as interactive turns). A made-up warmup
/// originator is not first-party and can be rejected independently of ChatGPT bearer auth.
fn warmup_originator() -> String {
    codex_login::default_client::originator().value
}

/// Spawns the periodic standby-window warmup loop. The caller owns the handle and may abort it.
pub(crate) fn spawn_window_warmup_task(
    pool: Arc<AccountPool>,
    mut config_rx: watch::Receiver<Config>,
) -> JoinHandle<()> {
    record_window_warmup_debug(WindowWarmupDebugKind::TaskSpawned);
    tokio::spawn(async move {
        let initial_deadline = tokio::time::Instant::now() + INITIAL_WARMUP_SETTLE;
        let mut last_pass = None;
        loop {
            let config = config_rx.borrow_and_update().clone();
            let sleep_for = if pool.needs_urgent_window_warmup(
                config.account_pool.effective_preemptive_switch_percent(),
            ) {
                URGENT_WARMUP_INTERVAL.min(config.account_pool.effective_window_warmup_interval())
            } else {
                config.account_pool.effective_window_warmup_interval()
            };
            let deadline =
                last_pass.map_or(initial_deadline, |completed_at| completed_at + sleep_for);
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    let config = config_rx.borrow_and_update().clone();
                    if !config.account_pool.effective_window_warmup() {
                        record_window_warmup_debug(WindowWarmupDebugKind::WarmupDisabled);
                        return;
                    }
                    if let Err(error) = run_warmup_pass(&pool, &config).await {
                        debug!(error = %error, "account window warmup pass failed");
                        record_window_warmup_debug(WindowWarmupDebugKind::PassFailed {
                            error: error.to_string(),
                        });
                    }
                    last_pass = Some(tokio::time::Instant::now());
                }
                changed = config_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    })
}

enum WarmupAttemptOutcome {
    Started,
    Unconfirmed,
    Failed,
    SkippedNoAuth,
}

pub(crate) async fn run_warmup_pass(pool: &AccountPool, config: &Config) -> anyhow::Result<()> {
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        record_window_warmup_debug(WindowWarmupDebugKind::WarmupDisabled);
        return Ok(());
    }
    record_window_warmup_debug(WindowWarmupDebugKind::PassBegin);
    let store = AccountRuntimeStateStore::new(config.codex_home.to_path_buf());
    let lock_store = store.clone();
    let _lock = tokio::task::spawn_blocking(move || lock_store.lock_window_warmup())
        .await
        .map_err(|error| anyhow::anyhow!("window warmup lock join failed: {error}"))??;
    if codex_login::AccountPoolRuntime::is_home_suspended(&config.codex_home) {
        return Ok(());
    }
    // Import shared observations without changing the foreground execution selection.
    for profile in store.load()?.profiles {
        let _ = pool.update_rate_limits(&profile.profile_id, profile.rate_limits);
    }
    store.apply_window_warmup_to_pool(pool)?;
    let Some(profile_id) = pool.window_warmup_candidates().into_iter().next() else {
        record_window_warmup_debug(WindowWarmupDebugKind::PassNoCandidate);
        return Ok(());
    };
    let Some((_, auth_manager)) = pool
        .auth_managers()
        .into_iter()
        .find(|(id, _)| id == &profile_id)
    else {
        return Ok(());
    };
    let attempted_at = Utc::now();
    let consecutive_failures = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id == profile_id)
        .and_then(|snapshot| snapshot.window_warmup)
        .map_or(1, |observation| {
            observation.consecutive_failures.saturating_add(1)
        });
    let retry_minutes = (5_i64 * (1_i64 << consecutive_failures.saturating_sub(1).min(4))).min(60);
    let pending = WindowWarmupObservation::current(
        WindowWarmupOutcome::Failed,
        attempted_at,
        Some(attempted_at + chrono::Duration::minutes(retry_minutes)),
        consecutive_failures,
    );
    // Persist before POST: cancellation and process exit must not erase an attempted request.
    store.record_window_warmup(&profile_id, pending.clone())?;
    pool.record_window_warmup(&profile_id, pending)?;

    let result = warm_profile(pool, config, &profile_id, auth_manager).await;
    let (outcome, retry_after, failures) = match &result {
        Ok(WarmupAttemptOutcome::Started) => (WindowWarmupOutcome::Succeeded, None, 0),
        Ok(WarmupAttemptOutcome::Unconfirmed) => (
            WindowWarmupOutcome::Failed,
            // Completion proves that a generating request already ran. Rounded or delayed
            // 0% usage is not grounds to spend quota again every few minutes. Keep the
            // start unconfirmed, but share one full-window retry deadline across processes.
            Some(attempted_at + chrono::Duration::hours(5)),
            0,
        ),
        Ok(WarmupAttemptOutcome::SkippedNoAuth) => (
            WindowWarmupOutcome::SkippedNoAuth,
            Some(Utc::now() + chrono::Duration::minutes(10)),
            0,
        ),
        Ok(WarmupAttemptOutcome::Failed) | Err(_) => (
            WindowWarmupOutcome::Failed,
            Some(Utc::now() + chrono::Duration::minutes(retry_minutes)),
            consecutive_failures,
        ),
    };
    let observation =
        WindowWarmupObservation::current(outcome, attempted_at, retry_after, failures);
    pool.record_window_warmup(&profile_id, observation.clone())?;
    store.record_window_warmup(&profile_id, observation)?;
    if let Some(snapshot) = pool
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.profile.id == profile_id)
    {
        store.record_rate_limits(&profile_id, snapshot.rate_limits)?;
    }
    result.map(|_| ())
}

#[cfg(test)]
#[path = "account_window_warmup_tests.rs"]
mod tests;
