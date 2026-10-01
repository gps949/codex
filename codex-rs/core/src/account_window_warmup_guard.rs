//! Publishes duplicate-generation protection at the actual transport send boundary.

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountProfileId;
use codex_login::AccountRuntimeStateStore;

#[derive(Debug)]
pub(crate) struct WarmupRequestGuard {
    store: AccountRuntimeStateStore,
    profile_id: AccountProfileId,
    attempted_at: DateTime<Utc>,
    sent: AtomicBool,
}

impl WarmupRequestGuard {
    pub(crate) fn new(
        store: AccountRuntimeStateStore,
        profile_id: AccountProfileId,
        attempted_at: DateTime<Utc>,
    ) -> Self {
        Self {
            store,
            profile_id,
            attempted_at,
            sent: AtomicBool::new(false),
        }
    }

    pub(crate) fn attempted_at(&self) -> DateTime<Utc> {
        self.attempted_at
    }

    pub(crate) fn may_have_been_sent(&self) -> bool {
        self.sent.load(Ordering::SeqCst)
    }

    pub(crate) fn before_send(&self) -> io::Result<()> {
        if self.may_have_been_sent() {
            return Err(io::Error::other(
                "uncertain warmup generation must not switch transports and resend",
            ));
        }
        if !self
            .store
            .claim_window_warmup(&self.profile_id, self.attempted_at)
            .map_err(io::Error::other)?
        {
            return Err(io::Error::other(
                "warmup send lost its durable standby claim",
            ));
        }
        self.sent.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// A received rejection proves this attempt did not generate; same-seat recovery may retry.
    pub(crate) fn definite_rejection(&self) {
        self.sent.store(false, Ordering::SeqCst);
    }
}
