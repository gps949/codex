//! Presents persisted warmup evidence without sending a generating request.

use super::ManagedWarmupView;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_login::AccountRateLimits;
use codex_login::WindowWarmupObservation;
use codex_login::WindowWarmupOutcome;
use codex_login::WindowWarmupPhase;

pub(super) fn view(
    observation: &WindowWarmupObservation,
    limits: &AccountRateLimits,
    now: DateTime<Utc>,
) -> ManagedWarmupView {
    let age = now.signed_duration_since(observation.attempted_at);
    let active_window = limits.primary.as_ref().is_some_and(|window| {
        window.used_percent.is_finite()
            && window.used_percent > 0.0
            && window.resets_at.is_some_and(|reset| reset > now)
            && limits.primary_observed_at().is_some_and(|at| {
                at.timestamp() <= now.timestamp()
                    && now.signed_duration_since(at) < Duration::hours(5)
            })
    });
    let status = if age < -Duration::minutes(5) || age >= Duration::hours(5) {
        "expired"
    } else if active_window {
        // Positive current usage is stronger evidence than an earlier failed warmup attempt.
        "windowActive"
    } else {
        let phase = observation.phase.or_else(|| {
            (observation.outcome == WindowWarmupOutcome::Failed
                && observation.consecutive_failures == 0
                && observation.retry_after.is_some_and(|retry| {
                    retry.signed_duration_since(observation.attempted_at) == Duration::hours(5)
                }))
            .then_some(WindowWarmupPhase::Unconfirmed)
        });
        match phase {
            Some(WindowWarmupPhase::InProgress) if age < Duration::minutes(3) => "inProgress",
            Some(WindowWarmupPhase::InProgress | WindowWarmupPhase::Unconfirmed) => "unconfirmed",
            None => match observation.outcome {
                WindowWarmupOutcome::Succeeded => "completedUnconfirmed",
                WindowWarmupOutcome::SkippedNoAuth => "needsLogin",
                WindowWarmupOutcome::Failed => match observation.retry_after {
                    Some(retry) if retry > now && observation.consecutive_failures == 0 => {
                        "deferred"
                    }
                    Some(retry) if retry > now => "failed",
                    Some(_) | None => "retryReady",
                },
            },
        }
    };
    ManagedWarmupView {
        status: status.into(),
        attempted_at: observation.attempted_at.timestamp(),
        retry_after: observation.retry_after.map(|at| at.timestamp()),
        consecutive_failures: observation.consecutive_failures,
    }
}

#[cfg(test)]
#[path = "warmup_tests.rs"]
mod tests;
