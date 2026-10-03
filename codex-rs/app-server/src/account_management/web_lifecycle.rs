//! Tab leases keep browser exit, refresh and process shutdown separate.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

const LEASE_LIFETIME: Duration = Duration::from_secs(5 * 60);
const CLOSE_GRACE: Duration = Duration::from_secs(30);
const STARTUP_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_TABS: usize = 128;

#[derive(Clone)]
pub(super) struct WebLifecycle {
    inner: Arc<Mutex<LeaseState>>,
    shutdown: CancellationToken,
}

struct LeaseState {
    started: Instant,
    tabs: HashMap<String, TabLease>,
    paired: bool,
    empty_since: Option<Instant>,
    activities: usize,
}

struct TabLease {
    updated: Instant,
    closed: bool,
}

impl WebLifecycle {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LeaseState {
                started: now,
                tabs: HashMap::new(),
                paired: false,
                empty_since: None,
                activities: 0,
            })),
            shutdown: CancellationToken::new(),
        }
    }

    pub(super) fn shutdown(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub(super) fn renew(&self, tab: &str, now: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(uuid::Uuid::parse_str(tab).is_ok(), "Invalid browser tab ID");
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            !self.shutdown.is_cancelled(),
            "Account manager is shutting down"
        );
        state
            .tabs
            .retain(|_, lease| now.saturating_duration_since(lease.updated) < LEASE_LIFETIME);
        anyhow::ensure!(
            state.tabs.get(tab).is_none_or(|lease| !lease.closed),
            "This browser tab has left the manager"
        );
        anyhow::ensure!(
            state.tabs.contains_key(tab) || state.tabs.len() < MAX_TABS,
            "Too many account manager tabs"
        );
        state.tabs.insert(
            tab.into(),
            TabLease {
                updated: now,
                closed: false,
            },
        );
        state.paired = true;
        state.empty_since = None;
        Ok(())
    }

    pub(super) fn leave(&self, tab: &str, now: Instant) -> anyhow::Result<()> {
        anyhow::ensure!(uuid::Uuid::parse_str(tab).is_ok(), "Invalid browser tab ID");
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .tabs
            .retain(|_, lease| now.saturating_duration_since(lease.updated) < LEASE_LIFETIME);
        anyhow::ensure!(
            state.tabs.contains_key(tab) || state.tabs.len() < MAX_TABS,
            "Too many account manager tabs"
        );
        // A page can leave before its first heartbeat arrives. Retain that tombstone too.
        state.tabs.insert(
            tab.into(),
            TabLease {
                updated: now,
                closed: true,
            },
        );
        state.paired = true;
        if state.tabs.values().all(|lease| lease.closed) && state.empty_since.is_none() {
            state.empty_since = Some(now);
        }
        Ok(())
    }

    pub(super) fn activity(&self) -> anyhow::Result<WebActivity> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        anyhow::ensure!(
            !self.shutdown.is_cancelled(),
            "Account manager is shutting down"
        );
        state.paired = true;
        state.activities += 1;
        if state.tabs.values().all(|lease| lease.closed) {
            state.empty_since = Some(Instant::now());
        }
        Ok(WebActivity {
            lifecycle: self.clone(),
        })
    }

    fn check_idle(&self, now: Instant) -> bool {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .tabs
            .retain(|_, lease| now.saturating_duration_since(lease.updated) < LEASE_LIFETIME);
        if state.tabs.values().any(|lease| !lease.closed) {
            state.empty_since = None;
            return false;
        }
        let idle = if state.paired {
            let empty_since = *state.empty_since.get_or_insert(now);
            now.saturating_duration_since(empty_since) >= CLOSE_GRACE
        } else {
            now.saturating_duration_since(state.started) >= STARTUP_LIFETIME
        };
        if idle && state.activities == 0 {
            self.shutdown.cancel();
            return true;
        }
        false
    }

    pub(super) async fn wait(&self) {
        let mut clock = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = clock.tick() => {
                    if self.check_idle(Instant::now()) {
                        return;
                    }
                }
            }
        }
    }
}

pub(super) struct WebActivity {
    lifecycle: WebLifecycle,
}

impl Drop for WebActivity {
    fn drop(&mut self) {
        let mut state = self
            .lifecycle
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.activities -= 1;
    }
}

#[cfg(test)]
#[path = "web_lifecycle_tests.rs"]
mod tests;
