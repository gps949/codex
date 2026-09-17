//! Process-local ring of standby-window warmup events for `/warmup`.
//!
//! Failures stay off the account picker. This buffer is the screenshotable
//! source of truth for why a pass ran, skipped, or did not start the 5h window.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::DateTime;
use chrono::Utc;

/// Newest events stay; older ones drop once the ring is full.
const MAX_EVENTS: usize = 80;
const MAX_MESSAGE_CHARS: usize = 400;

/// One structured warmup event that later formats to a single dump line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WindowWarmupDebugKind {
    TaskSpawned,
    TaskStopped,
    WarmupDisabled,
    PassBegin,
    PassNoCandidate,
    PassFailed {
        error: String,
    },
    SkipNoAuth {
        profile_id: String,
    },
    CatalogEmpty {
        profile_id: String,
    },
    CatalogResolved {
        profile_id: String,
        slugs: Vec<String>,
    },
    RequestStart {
        profile_id: String,
        model: String,
        effort: String,
        use_responses_lite: bool,
    },
    ModelRejected {
        profile_id: String,
        rejected_model: String,
        fallback_model: String,
        error: String,
    },
    RequestFailed {
        profile_id: String,
        error: String,
    },
    RequestTimeout {
        profile_id: String,
    },
    Noop {
        profile_id: String,
        stream_started: bool,
        get_primary: Option<String>,
    },
    Succeeded {
        profile_id: String,
        used_percent: String,
    },
    RunNowRequested,
}

impl WindowWarmupDebugKind {
    /// Compact one-line form for the `/warmup` transcript dump.
    pub fn line(&self) -> String {
        match self {
            Self::TaskSpawned => "task spawned".to_string(),
            Self::TaskStopped => "task stopped".to_string(),
            Self::WarmupDisabled => "warmup disabled in config".to_string(),
            Self::PassBegin => "pass begin".to_string(),
            Self::PassNoCandidate => "pass skipped: no idle standby".to_string(),
            Self::PassFailed { error } => format!("pass failed error={error}"),
            Self::SkipNoAuth { profile_id } => {
                format!("skip standby: no ChatGPT auth profile={profile_id}")
            }
            Self::CatalogEmpty { profile_id } => {
                format!("catalog empty profile={profile_id}")
            }
            Self::CatalogResolved { profile_id, slugs } => {
                format!("catalog slugs={} profile={profile_id}", slugs.join(","))
            }
            Self::RequestStart {
                profile_id,
                model,
                effort,
                use_responses_lite,
            } => format!(
                "request start profile={profile_id} model={model} effort={effort} lite={use_responses_lite}"
            ),
            Self::ModelRejected {
                profile_id,
                rejected_model,
                fallback_model,
                error,
            } => format!(
                "model rejected profile={profile_id} rejected={rejected_model} fallback={fallback_model} error={error}"
            ),
            Self::RequestFailed { profile_id, error } => {
                format!("request failed profile={profile_id} error={error}")
            }
            Self::RequestTimeout { profile_id } => {
                format!("request timeout profile={profile_id}")
            }
            Self::Noop {
                profile_id,
                stream_started,
                get_primary,
            } => {
                let get_primary = get_primary.as_deref().unwrap_or("none");
                format!(
                    "noop profile={profile_id} stream_started={stream_started} get_primary={get_primary}"
                )
            }
            Self::Succeeded {
                profile_id,
                used_percent,
            } => format!("succeeded profile={profile_id} used_percent={used_percent}"),
            Self::RunNowRequested => "run now requested".to_string(),
        }
    }
}

/// Timestamped dump line stored in the process-local ring.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowWarmupDebugEvent {
    pub at: DateTime<Utc>,
    pub message: String,
}

/// Bounded in-memory log used by tests and the process-global ring.
#[derive(Debug)]
pub struct WindowWarmupDebugLog {
    events: VecDeque<WindowWarmupDebugEvent>,
}

impl WindowWarmupDebugLog {
    pub const fn new() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }

    pub fn record(&mut self, kind: WindowWarmupDebugKind) {
        self.record_at(Utc::now(), kind);
    }

    pub fn record_at(&mut self, at: DateTime<Utc>, kind: WindowWarmupDebugKind) {
        let message = truncate_debug_message(kind.line());
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events
            .push_back(WindowWarmupDebugEvent { at, message });
    }

    pub fn events(&self) -> Vec<WindowWarmupDebugEvent> {
        self.events.iter().cloned().collect()
    }
}

static LOG: Mutex<WindowWarmupDebugLog> = Mutex::new(WindowWarmupDebugLog::new());

/// Append one warmup event to the process-local ring.
pub fn record_window_warmup_debug(kind: WindowWarmupDebugKind) {
    if let Ok(mut log) = LOG.lock() {
        log.record(kind);
    }
}

/// Snapshot of the process-local ring, oldest first.
pub fn window_warmup_debug_events() -> Vec<WindowWarmupDebugEvent> {
    LOG.lock().map(|log| log.events()).unwrap_or_default()
}

fn truncate_debug_message(message: String) -> String {
    if message.chars().count() <= MAX_MESSAGE_CHARS {
        return message;
    }
    message.chars().take(MAX_MESSAGE_CHARS).collect()
}

#[cfg(test)]
#[path = "account_window_warmup_debug_tests.rs"]
mod tests;
