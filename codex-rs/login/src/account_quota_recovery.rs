//! Separates scheduler retry timing from backend evidence of a natural reset.

use chrono::DateTime;
use chrono::Utc;

use super::AccountAvailability;
use super::AccountAvailabilityMutation;
use super::AccountLease;
use super::AccountPool;
use super::AccountPoolError;
use super::AccountRateLimits;

/// The reason for a quota deadline; a local probe does not establish a natural reset.
#[derive(Clone, Copy, Debug)]
pub enum AccountQuotaRecovery {
    BackendReset {
        resets_at: DateTime<Utc>,
        retry_at: DateTime<Utc>,
    },
    Reprobe {
        retry_at: DateTime<Utc>,
    },
    Unscheduled,
}

#[cfg(test)]
#[path = "account_quota_recovery_tests.rs"]
mod tests;

pub(super) enum BackendResetEvidence {
    Known(DateTime<Utc>),
    Unknown,
}

impl AccountPool {
    /// Atomically records recovery timing and optional quota on the request-bound lease.
    pub fn mark_exhausted_for_recovery(
        &self,
        lease: &AccountLease,
        recovery: AccountQuotaRecovery,
        rate_limits: Option<AccountRateLimits>,
    ) -> Result<AccountAvailabilityMutation, AccountPoolError> {
        let (retry_at, evidence) = match recovery {
            AccountQuotaRecovery::BackendReset {
                resets_at,
                retry_at,
            } => (Some(retry_at), BackendResetEvidence::Known(resets_at)),
            AccountQuotaRecovery::Reprobe { retry_at } => {
                (Some(retry_at), BackendResetEvidence::Unknown)
            }
            AccountQuotaRecovery::Unscheduled => (None, BackendResetEvidence::Unknown),
        };
        self.mark_unavailable_from_lease(
            lease,
            AccountAvailability::Exhausted {
                resets_at: retry_at,
            },
            rate_limits,
            evidence,
        )
    }
}
