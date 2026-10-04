//! Reports saved host sign-in separately from recent live Remote observations.

use super::AccountManager;
use codex_login::PrimaryLoginStatus;
use codex_login::PrimaryRuntimeStatusStore;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryLoginView {
    pub source: String,
    pub profile_id: Option<String>,
    pub label: String,
    pub email: Option<String>,
    pub ready: bool,
    pub message: Option<String>,
    pub status: String,
    pub revision: u64,
    pub runtime: Option<PrimaryRuntimeView>,
}

/// A live host heartbeat; stored credentials never imply this connection state.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryRuntimeView {
    pub source_revision: u64,
    pub email: Option<String>,
    pub profile_id: Option<String>,
    pub remote_status: String,
    pub observed_at: i64,
}

impl AccountManager {
    pub(super) fn primary_login_view(&self) -> PrimaryLoginView {
        let observation = codex_login::observe_primary_login(&self.config.auth_config());
        let runtime = if observation.status == PrimaryLoginStatus::Invalid {
            None
        } else {
            PrimaryRuntimeStatusStore::read_current(&self.config.codex_home, observation.revision)
                .map(|snapshot| PrimaryRuntimeView {
                    source_revision: snapshot.source_revision,
                    email: snapshot.email,
                    profile_id: snapshot.profile_id,
                    remote_status: snapshot.remote_status.as_str().into(),
                    observed_at: snapshot.observed_at,
                })
        };
        PrimaryLoginView {
            source: observation.source,
            profile_id: observation.profile_id,
            label: observation.label,
            email: observation.email,
            ready: observation.status == PrimaryLoginStatus::StoredReady,
            message: observation.message,
            status: observation.status.as_str().into(),
            revision: observation.revision,
            runtime,
        }
    }
}
