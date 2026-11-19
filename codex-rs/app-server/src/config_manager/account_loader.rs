//! Rebind ChatGPT policy when a captured inference or host credential source changes.

use super::*;

impl ConfigManager {
    pub(crate) fn replace_cloud_config_bundle_loader(
        &self,
        auth_manager: Arc<AuthManager>,
        chatgpt_base_url: String,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) {
        // Config-server authentication belongs to its managed binding, not pool rotation.
        if self
            .current_managed_config_loader()
            .config_server()
            .is_some()
        {
            self.revoke_mcp_ema();
            return;
        }
        let loader = codex_cloud_config::chatgpt_config_bundle_loader(
            auth_manager,
            chatgpt_base_url,
            self.codex_home.clone(),
            http_client_factory.with_network_policy(self.local_network_policy.policy()),
        );
        if let Ok(mut current) = self.cloud_config_bundle.write() {
            current.retire_ema_policy();
            *current = loader;
        } else {
            warn!("failed to replace account cloud config loader");
        }
        self.revoke_mcp_ema();
    }
}
