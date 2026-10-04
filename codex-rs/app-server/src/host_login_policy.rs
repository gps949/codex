//! Publishes account-specific host network requirements without touching inference policy.

use crate::config_manager::ConfigManager;
use codex_login::AuthManager;
use codex_login::ExternalAuthFuture;
use codex_login::PrimaryLoginPolicyLoader;
use std::io;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;
use tokio::sync::Mutex;

pub(crate) struct HostLoginPolicyLoader {
    pub(crate) config: ConfigManager,
    pub(crate) source: Arc<Mutex<Weak<AuthManager>>>,
    pub(crate) chatgpt_base_url: String,
    pub(crate) http_client_factory: codex_http_client::HttpClientFactory,
}

impl PrimaryLoginPolicyLoader for HostLoginPolicyLoader {
    fn prepare(&self, source_manager: Arc<AuthManager>) -> ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            let result = async {
                let mut source = Arc::clone(&self.source).lock_owned().await;
                if source
                    .upgrade()
                    .is_none_or(|previous| !Arc::ptr_eq(&previous, &source_manager))
                {
                    self.config.replace_cloud_config_bundle_loader(
                        Arc::clone(&source_manager),
                        self.chatgpt_base_url.clone(),
                        self.http_client_factory.clone(),
                    );
                    *source = Arc::downgrade(&source_manager);
                }
                let config = tokio::time::timeout(
                    Duration::from_secs(/*secs*/ 4),
                    self.config.load_latest_config(/*fallback_cwd*/ None),
                )
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Host network requirements timed out",
                    )
                })??;
                if config
                    .config_layer_stack
                    .requirements()
                    .allow_remote_control
                    .as_ref()
                    .is_some_and(|requirement| !requirement.value)
                {
                    return Err(
                        codex_login::PrimaryLoginPolicyFailure::RemoteDisabled.into_io_error()
                    );
                }
                let auth = source_manager.auth_cached().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotConnected,
                        "Selected host authentication is unavailable",
                    )
                })?;
                if !config.auth_config().allows_auth(&auth) {
                    return Err(codex_login::PrimaryLoginPolicyFailure::AuthenticationDenied
                        .into_io_error());
                }
                Ok(())
            }
            .await;
            if result.is_err() {
                self.config.suspend_host_login_requests();
            }
            result
        })
    }
}

#[cfg(test)]
#[path = "host_login_policy_tests.rs"]
mod tests;
