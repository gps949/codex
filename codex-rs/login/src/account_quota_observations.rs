//! Per-window quota observations with a separate snapshot ordering watermark.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

/// One rate-limit window reported for an account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountRateLimitWindow {
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
    /// Rolling window length in minutes when the backend reports it (5h primary is `300`).
    #[serde(default)]
    pub window_minutes: Option<i64>,
}

/// Independent times for accepted window evidence; a missing time stays unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountWindowObservationTimes {
    pub primary: Option<DateTime<Utc>>,
    pub secondary: Option<DateTime<Utc>>,
}

/// Cached quota is advisory; real request failures remain authoritative for availability.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountRateLimits {
    pub primary: Option<AccountRateLimitWindow>,
    pub secondary: Option<AccountRateLimitWindow>,
    /// Latest accepted snapshot time, not proof that every retained window was refreshed.
    pub observed_at: Option<DateTime<Utc>>,
    /// None preserves the legacy shared timestamp. Some with a null member preserves an
    /// explicitly unknown window time instead of borrowing a newer snapshot's timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_observed_at: Option<AccountWindowObservationTimes>,
}

impl AccountRateLimits {
    pub(crate) fn discard_windows_before(&mut self, reset_at: DateTime<Utc>) {
        let primary = self.primary_observed_at();
        let secondary = self.secondary_observed_at();
        if primary.is_none_or(|observed| observed <= reset_at) {
            self.primary = None;
        }
        if secondary.is_none_or(|observed| observed <= reset_at) {
            self.secondary = None;
        }
        let times = AccountWindowObservationTimes {
            primary: self.primary.as_ref().and(primary),
            secondary: self.secondary.as_ref().and(secondary),
        };
        let shared = self
            .primary
            .as_ref()
            .is_none_or(|_| times.primary == self.observed_at)
            && self
                .secondary
                .as_ref()
                .is_none_or(|_| times.secondary == self.observed_at);
        self.window_observed_at = (!shared).then_some(times);
    }

    pub fn primary_observed_at(&self) -> Option<DateTime<Utc>> {
        self.primary.as_ref()?;
        self.window_observed_at
            .map_or(self.observed_at, |times| times.primary)
    }

    pub fn secondary_observed_at(&self) -> Option<DateTime<Utc>> {
        self.secondary.as_ref()?;
        self.window_observed_at
            .map_or(self.observed_at, |times| times.secondary)
    }
}
