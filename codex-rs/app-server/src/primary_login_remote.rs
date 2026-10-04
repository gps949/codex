//! Continues explicitly selected host identities while retiring old relay authorization.

use codex_app_server_transport::RemoteControlHandle;
use codex_app_server_transport::RemoteControlHandover;
use codex_login::CodexAuth;
use codex_login::PrimaryLoginRuntime;
use codex_login::PrimaryLoginState;
use codex_login::PrimaryLoginStore;
use codex_login::PrimaryLoginTransitionObserver;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

pub(crate) struct PrimaryRemoteObserver {
    config: codex_login::AuthConfig,
    runtime: Weak<PrimaryLoginRuntime>,
    owner: Weak<Self>,
    remote: RemoteControlHandle,
    destination: String,
    state: Mutex<ObservedSelection>,
    status: Option<codex_login::PrimaryRuntimeStatusStore>,
}

struct ObservedSelection {
    revision: u64,
    pending: Option<PendingContinuation>,
    in_flight: bool,
}

#[derive(Clone)]
struct PendingContinuation {
    selection: PrimaryLoginState,
    remote: RemoteControlHandover,
}

impl PrimaryRemoteObserver {
    pub(crate) fn new(
        config: codex_login::AuthConfig,
        runtime: &Arc<PrimaryLoginRuntime>,
        remote: RemoteControlHandle,
        destination: String,
    ) -> Arc<Self> {
        let revision = PrimaryLoginStore::new(config.codex_home.clone())
            .load()
            .map_or(0, |state| state.revision);
        let status = codex_login::PrimaryRuntimeStatusStore::new(config.codex_home.clone()).ok();
        Arc::new_cyclic(|owner| Self {
            config,
            status,
            runtime: Arc::downgrade(runtime),
            owner: owner.clone(),
            remote,
            destination,
            state: Mutex::new(ObservedSelection {
                revision,
                pending: None,
                in_flight: false,
            }),
        })
    }
}

impl PrimaryLoginTransitionObserver for PrimaryRemoteObserver {
    fn selection_unavailable(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending = None;
        if let Some(status) = &self.status {
            let _ = status.clear();
        }
    }

    fn before_selection(&self, selection: &PrimaryLoginState) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if selection.revision == state.revision {
            return;
        }
        state.revision = selection.revision;
        let old = state.pending.take();
        let Some(intent) = selection
            .remote_handover
            .as_ref()
            .filter(|intent| intent.is_current())
        else {
            return;
        };
        let continuation = self.remote.prepare_primary_login_handover().or_else(|| {
            old.filter(|pending| {
                pending
                    .selection
                    .remote_handover
                    .as_ref()
                    .is_some_and(codex_login::PrimaryLoginHandover::is_current)
            })
            .map(|pending| pending.remote)
        });
        if intent.is_current()
            && let Some(remote) = continuation
        {
            state.pending = Some(PendingContinuation {
                selection: selection.clone(),
                remote,
            });
        }
    }

    fn after_selection(&self, selection: &PrimaryLoginState, auth: Option<&CodexAuth>) {
        if let Some(writer) = &self.status {
            let remote = match self.remote.status().status {
                codex_app_server_protocol::RemoteControlConnectionStatus::Disabled => {
                    codex_login::PrimaryRemoteStatus::Disabled
                }
                codex_app_server_protocol::RemoteControlConnectionStatus::Connecting => {
                    codex_login::PrimaryRemoteStatus::Connecting
                }
                codex_app_server_protocol::RemoteControlConnectionStatus::Connected => {
                    codex_login::PrimaryRemoteStatus::Connected
                }
                codex_app_server_protocol::RemoteControlConnectionStatus::Errored => {
                    codex_login::PrimaryRemoteStatus::Errored
                }
            };
            let remote = match self
                .runtime
                .upgrade()
                .and_then(|runtime| runtime.policy_failure(selection.revision))
            {
                Some(codex_login::PrimaryLoginPolicyFailure::RemoteDisabled) => {
                    codex_login::PrimaryRemoteStatus::RequirementsDisabled
                }
                Some(codex_login::PrimaryLoginPolicyFailure::AuthenticationDenied) => {
                    codex_login::PrimaryRemoteStatus::AuthenticationDenied
                }
                None => remote,
            };
            let profile_id = match &selection.source {
                codex_login::PrimaryLoginSource::Profile { profile_id, .. } => {
                    Some(profile_id.to_string())
                }
                codex_login::PrimaryLoginSource::RootLogin
                | codex_login::PrimaryLoginSource::SignedOut => None,
            };
            let _ = writer.publish(&codex_login::PrimaryRuntimeSnapshot {
                source_revision: selection.revision,
                email: auth.and_then(CodexAuth::get_account_email),
                profile_id,
                remote_status: remote,
                observed_at: chrono::Utc::now().timestamp(),
            });
        }
        let Some(intent) = selection
            .remote_handover
            .as_ref()
            .filter(|intent| intent.is_current())
        else {
            return;
        };
        if auth.is_none_or(|auth| !intent.matches_auth(auth)) {
            self.selection_unavailable();
            return;
        }
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.in_flight
                || state
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.selection != *selection)
            {
                return;
            }
            state.in_flight = true;
            state.pending.clone()
        };
        let Some(pending) = pending else {
            return;
        };
        let Some(observer) = self.owner.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            let completed =
                tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 6), async {
                    let runtime = observer.runtime.upgrade()?;
                    let manager = runtime.auth_manager();
                    let Some((auth, factory)) = manager.auth_with_http_client_factory().await
                    else {
                        // A confirmed deny is terminal for this one-time intent. A later explicit
                        // selection or Remote command can try again after requirements change.
                        return runtime
                            .policy_failure(pending.selection.revision)
                            .map(|_| true);
                    };
                    if runtime.policy_failure(pending.selection.revision).is_some()
                        || pending
                            .selection
                            .remote_handover
                            .as_ref()
                            .is_none_or(|intent| {
                                !intent.is_current() || !intent.matches_auth(&auth)
                            })
                    {
                        return Some(true);
                    }
                    let Ok(destination) = observer.destination.parse() else {
                        return Some(true);
                    };
                    if factory.network_policy().acquire(&destination).is_err() {
                        return None;
                    }
                    let store = PrimaryLoginStore::new(observer.config.codex_home.clone());
                    // Selection and exact stored owner remain locked until the synchronous Remote
                    // decision commits. A newer selection or replacement credential cancels this job.
                    let continued = store.with_current_handover(
                        &observer.config,
                        &pending.selection,
                        |intent| {
                            observer
                                .remote
                                .continue_primary_login_handover(&pending.remote, intent)
                                .map_err(std::io::Error::other)
                        },
                    );
                    Some(
                        continued.is_ok()
                            || continued.is_err_and(|error| {
                                error.kind() == std::io::ErrorKind::InvalidData
                            }),
                    )
                })
                .await
                .ok()
                .flatten()
                .unwrap_or(false);
            let mut state = observer
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.in_flight = false;
            if state
                .pending
                .as_ref()
                .is_some_and(|current| current.selection == pending.selection)
            {
                // Successful continuation consumes the intent. Temporary policy failures retry
                // only within its bounded lifetime and never perform inference or redemption.
                let status = observer.remote.status();
                if completed
                    || matches!(
                        status.status,
                        codex_app_server_protocol::RemoteControlConnectionStatus::Connecting
                            | codex_app_server_protocol::RemoteControlConnectionStatus::Connected
                    )
                    || pending
                        .selection
                        .remote_handover
                        .as_ref()
                        .is_none_or(|intent| !intent.is_current())
                {
                    state.pending = None;
                }
            }
        });
    }
}
