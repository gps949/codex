use chrono::DateTime;
use chrono::Utc;

/// Chooses a display name without changing the profile's stored custom label.
///
/// Renderers remain responsible for escaping controls and fitting terminal or UI widths.
pub fn account_display_name<'a>(
    label: Option<&'a str>,
    email: Option<&'a str>,
    profile_id: &'a str,
) -> &'a str {
    label
        .into_iter()
        .chain(email)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(profile_id)
}

/// Formats an exhausted-until / reset timestamp for CLI-style surfaces.
///
/// Prefer [`format_primary_window_reset`] when rendering primary quota windows so idle
/// `0%` accounts are not shown with a fake countdown.
pub fn format_exhausted_reset(reset: DateTime<Utc>) -> String {
    let absolute = reset.format("%Y-%m-%d %H:%M UTC");
    let remaining = reset.signed_duration_since(Utc::now());
    if remaining.num_seconds() <= 0 {
        return absolute.to_string();
    }

    format!(
        "{absolute} (in {})",
        format_reset_countdown(remaining.num_seconds() as u64)
    )
}

pub fn format_exhausted_reset_unix(unix: i64) -> String {
    match DateTime::<Utc>::from_timestamp(unix, 0) {
        Some(reset) => format_exhausted_reset(reset),
        None => format!("unix:{unix}"),
    }
}

/// Compact remaining-time countdown: `H:MM`, with explicit units when ≥ 24h.
pub fn format_reset_countdown(remaining_seconds: u64) -> String {
    let total_minutes = remaining_seconds.div_ceil(60).max(1);
    let days = total_minutes / (24 * 60);
    let hours = (total_minutes / 60) % 24;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}:{minutes:02}")
    }
}

/// Formats the primary (5h) window reset line.
///
/// Zero observed usage cannot confirm whether a tiny request started the window.
/// The backend can return a full-window reset even for idle accounts, so do not
/// present that reset as evidence of a ticking clock.
pub fn format_primary_window_reset(
    used_percent: f64,
    reset_at: Option<i64>,
    now: DateTime<Utc>,
) -> String {
    if used_percent <= 0.0 {
        return "start unconfirmed".to_string();
    }

    let Some(reset_at) = reset_at else {
        return "unknown".to_string();
    };
    let remaining_seconds = reset_at - now.timestamp();
    if remaining_seconds <= 0 {
        return "due".to_string();
    }

    format!("in {}", format_reset_countdown(remaining_seconds as u64))
}

/// Relative-only exhausted/cooldown countdown for compact UIs.
pub fn format_relative_reset(reset: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining_seconds = reset.signed_duration_since(now).num_seconds();
    if remaining_seconds <= 0 {
        return "due".to_string();
    }
    format!("in {}", format_reset_countdown(remaining_seconds as u64))
}

/// Compact status line for the latest standby 5h-window warmup observation.
///
/// Attempt phases distinguish completed requests from observed window starts.
/// Observations are bounded to a primary window so old errors do not remain forever.
pub fn format_window_warmup_status(
    observation: &crate::account_pool::WindowWarmupObservation,
    now: DateTime<Utc>,
) -> String {
    use crate::account_pool::WindowWarmupOutcome;
    use crate::account_pool::WindowWarmupPhase;

    let age = now.signed_duration_since(observation.attempted_at);
    if age < -chrono::Duration::minutes(5) || age >= chrono::Duration::hours(5) {
        return "warmup observation expired".to_string();
    }
    let phase = observation.phase.or_else(|| {
        (observation.outcome == WindowWarmupOutcome::Failed
            && observation.consecutive_failures == 0
            && observation.retry_after.is_some_and(|retry_after| {
                retry_after.signed_duration_since(observation.attempted_at)
                    == chrono::Duration::hours(5)
            }))
        .then_some(WindowWarmupPhase::Unconfirmed)
    });
    match phase {
        Some(WindowWarmupPhase::InProgress) if age < chrono::Duration::minutes(3) => {
            return "warmup in progress".to_string();
        }
        Some(WindowWarmupPhase::InProgress) => {
            return "warmup attempt; start unconfirmed".to_string();
        }
        Some(WindowWarmupPhase::Unconfirmed) => {
            return "warmup sent; start unconfirmed".to_string();
        }
        None => {}
    }
    match observation.outcome {
        WindowWarmupOutcome::Succeeded => "5h confirmed".to_string(),
        WindowWarmupOutcome::SkippedNoAuth => "warmup needs login".to_string(),
        WindowWarmupOutcome::Failed => match observation.retry_after {
            Some(retry_after) if retry_after > now && observation.consecutive_failures == 0 => {
                format!(
                    "warmup deferred; check {}",
                    format_relative_reset(retry_after, now)
                )
            }
            Some(retry_after) if retry_after > now => format!(
                "warmup failed; retry {}",
                format_relative_reset(retry_after, now)
            ),
            Some(_) | None => "warmup retry ready".to_string(),
        },
    }
}

/// Shows recent attempt status without confusing request completion with quota evidence.
/// Positive primary usage suppresses obsolete failures. Zero usage stays unconfirmed,
/// including when an earlier successful observation may have become stale.
pub fn visible_window_warmup_status(
    observation: &crate::account_pool::WindowWarmupObservation,
    primary_used_percent: Option<f64>,
    now: DateTime<Utc>,
) -> Option<String> {
    let age = now.signed_duration_since(observation.attempted_at);
    if age < -chrono::Duration::minutes(5) || age >= chrono::Duration::hours(5) {
        return None;
    }
    if primary_used_percent.is_some_and(|used| used > 0.0) {
        return (observation.outcome == crate::WindowWarmupOutcome::Succeeded)
            .then(|| "5h confirmed".to_string());
    }
    if observation.outcome == crate::WindowWarmupOutcome::Succeeded {
        return Some("warmup done; start unconfirmed".to_string());
    }
    Some(format_window_warmup_status(observation, now))
}

pub fn format_plan_type_label(plan_type: Option<&str>) -> String {
    match plan_type {
        Some(plan) if !plan.trim().is_empty() => plan.to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
#[path = "account_display_name_tests.rs"]
mod name_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_exhausted_reset_includes_absolute_and_relative() {
        let reset = Utc.with_ymd_and_hms(2099, 1, 2, 3, 4, 0).unwrap();
        let formatted = format_exhausted_reset(reset);
        assert!(formatted.starts_with("2099-01-02 03:04 UTC (in "));
        assert!(formatted.ends_with(')'));
    }

    #[test]
    fn format_primary_window_reset_does_not_infer_a_start_from_zero_usage() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        assert_eq!(
            format_primary_window_reset(0.0, Some(now.timestamp() + 5 * 3600), now),
            "start unconfirmed"
        );
        assert_eq!(
            format_primary_window_reset(38.0, Some(now.timestamp() + 3 * 3600 + 46 * 60), now),
            "in 3:46"
        );
        assert_eq!(format_primary_window_reset(10.0, None, now), "unknown");
    }

    #[test]
    fn format_reset_countdown_keeps_hours_compact_and_labels_days() {
        assert_eq!(format_reset_countdown(/*remaining_seconds*/ 1), "0:01");
        assert_eq!(
            format_reset_countdown(/*remaining_seconds*/ 3 * 60 * 60 + 46 * 60),
            "3:46"
        );
        assert_eq!(
            format_reset_countdown(
                /*remaining_seconds*/ 3 * 24 * 60 * 60 + 21 * 60 * 60 + 2 * 60
            ),
            "3d 21h 2m"
        );
    }

    #[test]
    fn format_window_warmup_status_summarizes_outcomes() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let succeeded = crate::account_pool::WindowWarmupObservation::current(
            crate::account_pool::WindowWarmupOutcome::Succeeded,
            now,
            None,
            /*consecutive_failures*/ 0,
        );
        assert_eq!(format_window_warmup_status(&succeeded, now), "5h confirmed");

        let failed = crate::account_pool::WindowWarmupObservation::current(
            crate::account_pool::WindowWarmupOutcome::Failed,
            now,
            Some(now + chrono::Duration::hours(6)),
            /*consecutive_failures*/ 5,
        );
        assert_eq!(
            format_window_warmup_status(&failed, now),
            "warmup failed; retry in 6:00"
        );
    }

    #[test]
    fn visible_window_warmup_status_distinguishes_completion_from_quota_evidence() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let succeeded = crate::account_pool::WindowWarmupObservation::current(
            crate::account_pool::WindowWarmupOutcome::Succeeded,
            now,
            None,
            /*consecutive_failures*/ 0,
        );
        assert_eq!(
            visible_window_warmup_status(&succeeded, /*primary_used_percent*/ Some(0.0), now),
            Some("warmup done; start unconfirmed".to_string())
        );
        assert_eq!(
            visible_window_warmup_status(&succeeded, /*primary_used_percent*/ Some(4.0), now),
            Some("5h confirmed".to_string())
        );

        let failed = crate::account_pool::WindowWarmupObservation::current(
            crate::account_pool::WindowWarmupOutcome::Failed,
            now,
            Some(now + chrono::Duration::minutes(2)),
            /*consecutive_failures*/ 1,
        );
        assert_eq!(
            visible_window_warmup_status(&failed, /*primary_used_percent*/ Some(4.0), now),
            None
        );
        assert_eq!(
            visible_window_warmup_status(&failed, /*primary_used_percent*/ Some(0.0), now),
            Some("warmup failed; retry in 0:02".to_string())
        );
    }

    #[test]
    fn phase_status_expires_and_interrupted_attempts_stop_showing_progress() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let mut pending = crate::WindowWarmupObservation::current(
            crate::WindowWarmupOutcome::Failed,
            now,
            Some(now + chrono::Duration::hours(5)),
            /*consecutive_failures*/ 0,
        );
        pending.phase = Some(crate::WindowWarmupPhase::InProgress);
        assert_eq!(
            (
                visible_window_warmup_status(&pending, Some(0.0), now),
                visible_window_warmup_status(
                    &pending,
                    Some(0.0),
                    now + chrono::Duration::minutes(4)
                ),
                visible_window_warmup_status(&pending, Some(0.0), now + chrono::Duration::hours(5)),
            ),
            (
                Some("warmup in progress".to_string()),
                Some("warmup attempt; start unconfirmed".to_string()),
                None,
            )
        );
        pending.phase = Some(crate::WindowWarmupPhase::Unconfirmed);
        assert_eq!(
            visible_window_warmup_status(&pending, Some(0.0), now),
            Some("warmup sent; start unconfirmed".to_string())
        );
    }

    #[test]
    fn legacy_unconfirmed_is_distinguished_from_a_real_failure_with_the_same_retry() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let legacy = crate::WindowWarmupObservation::current(
            crate::WindowWarmupOutcome::Failed,
            now,
            Some(now + chrono::Duration::hours(5)),
            /*consecutive_failures*/ 0,
        );
        let failed = crate::WindowWarmupObservation {
            consecutive_failures: 1,
            ..legacy
        };
        assert_eq!(
            (
                visible_window_warmup_status(&legacy, Some(0.0), now),
                visible_window_warmup_status(&failed, Some(0.0), now),
                visible_window_warmup_status(&failed, Some(0.0), now + chrono::Duration::hours(5)),
            ),
            (
                Some("warmup sent; start unconfirmed".to_string()),
                Some("warmup failed; retry in 5:00".to_string()),
                None,
            )
        );
    }

    #[test]
    fn expired_backoff_is_retry_ready_and_future_observations_are_hidden() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let failed = crate::WindowWarmupObservation::current(
            crate::WindowWarmupOutcome::Failed,
            now - chrono::Duration::minutes(3),
            Some(now - chrono::Duration::minutes(1)),
            /*consecutive_failures*/ 1,
        );
        let future = crate::WindowWarmupObservation {
            attempted_at: now + chrono::Duration::hours(1),
            ..failed
        };
        assert_eq!(
            (
                visible_window_warmup_status(&failed, None, now),
                visible_window_warmup_status(&future, None, now),
            ),
            (Some("warmup retry ready".to_string()), None)
        );
    }

    #[test]
    fn zero_failure_deferral_waits_without_claiming_failure_or_confirmation() {
        let now = Utc.with_ymd_and_hms(2026, 3, 17, 12, 0, 0).unwrap();
        let deferred = crate::WindowWarmupObservation::current(
            crate::WindowWarmupOutcome::Failed,
            now,
            Some(now + chrono::Duration::minutes(2)),
            /*consecutive_failures*/ 0,
        );
        assert_eq!(
            (
                visible_window_warmup_status(
                    &deferred,
                    /*primary_used_percent*/ Some(0.0),
                    now
                ),
                visible_window_warmup_status(&deferred, /*primary_used_percent*/ None, now),
                visible_window_warmup_status(
                    &deferred,
                    /*primary_used_percent*/ Some(4.0),
                    now
                ),
                visible_window_warmup_status(
                    &deferred,
                    /*primary_used_percent*/ Some(0.0),
                    now + chrono::Duration::minutes(2)
                ),
            ),
            (
                Some("warmup deferred; check in 0:02".to_string()),
                Some("warmup deferred; check in 0:02".to_string()),
                None,
                Some("warmup retry ready".to_string()),
            )
        );
    }
}
