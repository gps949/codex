use super::*;
use codex_login::ServerOptions;
use codex_protocol::config_types::ForcedLoginMethod;
use tokio_util::sync::CancellationToken;

pub(super) struct LoginJob {
    pub(super) progress: LoginProgress,
    pub(super) cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;

impl AccountManager {
    /// Reads device-login progress without contacting the backend or refreshing credentials.
    pub async fn login_progress(&self) -> Vec<LoginProgress> {
        let mut jobs: Vec<_> = self
            .logins
            .lock()
            .await
            .values()
            .map(|job| job.progress.clone())
            .collect();
        jobs.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));
        jobs
    }

    pub(super) async fn start_login(
        self: &Arc<Self>,
        profile_id: Option<String>,
        label: Option<String>,
    ) -> anyhow::Result<LoginProgress> {
        anyhow::ensure!(
            self.config
                .auth_config()
                .is_login_method_allowed(ForcedLoginMethod::Chatgpt),
            "ChatGPT login is disabled by the authentication policy"
        );
        let mut jobs = self.logins.lock().await;
        anyhow::ensure!(
            !self
                .login_shutdown
                .load(std::sync::atomic::Ordering::Acquire),
            "Account manager is shutting down"
        );
        jobs.retain(|_, job| job.task.as_ref().is_some_and(|task| !task.is_finished()));
        anyhow::ensure!(jobs.len() < 4, "Finish or cancel an existing login first");
        anyhow::ensure!(
            !profile_id
                .as_ref()
                .is_some_and(|id| jobs
                    .values()
                    .any(|job| job.progress.profile_id.as_ref() == Some(id))),
            "This account already has a login in progress"
        );
        // Reserve metadata and the job before starting device-code HTTP. No await separates
        // the four-job/same-profile checks from reservation publication.
        let store = self.store();
        let priority = store
            .load_profile_records()?
            .iter()
            .map(|record| record.profile.priority)
            .max()
            .unwrap_or(0)
            .saturating_add(10);
        let options = ServerOptions::new(
            self.config.codex_home.to_path_buf(),
            codex_login::CLIENT_ID.into(),
            self.config.auth_config().effective_chatgpt_workspaces(),
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
            self.config.auth_route_config(),
        );
        let pending = if let Some(id) = &profile_id {
            let profile = self.profile(id)?;
            codex_login::account_login::prepare_account_device_relogin(
                store,
                options,
                &profile.profile.id,
            )?
        } else {
            codex_login::account_login::prepare_account_device_login(
                store, options, label, priority,
            )?
        };
        let operation_id = uuid::Uuid::new_v4().to_string();
        let progress = LoginProgress {
            operation_id: operation_id.clone(),
            profile_id: Some(pending.profile().id.to_string()),
            verification_url: None,
            user_code: None,
            status: "waiting".into(),
            message: "Requesting browser verification code".into(),
        };
        let cancel = CancellationToken::new();
        let manager = Arc::clone(self);
        let task_cancel = cancel.clone();
        let task_id = operation_id.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let pending = pending
                    .request_code_with_cancellation(task_cancel.cancelled())
                    .await?;
                if let Some(job) = manager.logins.lock().await.get_mut(&task_id) {
                    job.progress.verification_url =
                        pending.verification_url().map(ToOwned::to_owned);
                    job.progress.user_code = pending.user_code().map(ToOwned::to_owned);
                    job.progress.message = "Waiting for browser verification".into();
                }
                pending
                    .complete_with_cancellation(task_cancel.cancelled())
                    .await
            }
            .await;
            if let Some(job) = manager.logins.lock().await.get_mut(&task_id) {
                match result {
                    Ok(outcome) => {
                        job.progress.status = "completed".into();
                        job.progress.profile_id = Some(outcome.profile.id.to_string());
                        job.progress.message = format!("Signed in: {}", outcome.profile.id);
                    }
                    Err(codex_login::AccountLoginFlowError::Cancelled) => {
                        job.progress.status = "cancelled".into();
                        job.progress.message = "Login cancelled".into();
                    }
                    Err(error) => {
                        job.progress.status = "failed".into();
                        job.progress.message = error.to_string();
                    }
                }
                job.progress.user_code = None;
                job.progress.verification_url = None;
            }
        });
        jobs.insert(
            operation_id,
            LoginJob {
                progress: progress.clone(),
                cancel,
                task: Some(task),
            },
        );
        Ok(progress)
    }

    /// Cancels outstanding HTTP waits and joins credential transactions before process exit.
    pub async fn shutdown_logins(&self) {
        self.login_shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        let tasks = {
            let mut jobs = self.logins.lock().await;
            jobs.values_mut()
                .filter_map(|job| {
                    job.cancel.cancel();
                    job.task.take()
                })
                .collect::<Vec<_>>()
        };
        for task in tasks {
            let _ = task.await;
        }
    }
}
