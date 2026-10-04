use super::*;

impl AuthConfig {
    /// Loads only persisted OAuth credentials for one explicitly selected pool profile.
    /// Host environment tokens and process-local external auth are never adopted.
    pub(crate) async fn load_managed_profile_auth(&self) -> std::io::Result<Option<CodexAuth>> {
        if self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
            return Ok(None);
        }
        let Some(saved) = load_auth_dot_json(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?
        else {
            return Ok(None);
        };
        if saved.resolved_mode() != AuthMode::Chatgpt {
            return Ok(None);
        }
        let auth = CodexAuth::from_auth_dot_json(
            &self.codex_home,
            saved,
            self.auth_credentials_store_mode,
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            /*agent_identity_authapi_base_url*/ None,
            &self.auth_route_config,
        )
        .await?;
        Ok(self.allows_auth(&auth).then_some(auth))
    }

    /// Initializing PAT and Agent Identity auth can require HTTP; idle polling must defer it.
    pub(crate) fn root_auth_load_is_local(&self) -> std::io::Result<bool> {
        if read_codex_access_token_from_env().is_some() {
            return Ok(false);
        }
        let saved = match load_auth_dot_json(
            &self.codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            self.keyring_backend_kind,
        )? {
            Some(auth) => Some(auth),
            None => load_auth_dot_json(
                &self.codex_home,
                self.auth_credentials_store_mode,
                self.keyring_backend_kind,
            )?,
        };
        Ok(saved.is_none_or(|auth| {
            !matches!(
                auth.resolved_mode(),
                AuthMode::PersonalAccessToken | AuthMode::AgentIdentity
            )
        }))
    }
}

impl AuthManager {
    /// Updates host-source credentials without losing its provider or revoking same-owner refreshes.
    pub(crate) fn sync_host_login_cached_auth(
        &self,
        auth: Option<CodexAuth>,
    ) -> std::io::Result<()> {
        if let Some(auth) = &auth {
            let allowed = self.allowed_login_methods();
            validate_auth_restrictions(
                Some(&allowed),
                self.effective_chatgpt_workspaces().as_deref(),
                auth,
            )
            .map_err(std::io::Error::other)?;
        }
        if let Ok(mut cached) = self.inner.write()
            && !Self::auths_equal(cached.auth.as_ref(), auth.as_ref())
        {
            cached.permanent_refresh_failure = None;
        }
        self.set_cached_auth(auth);
        Ok(())
    }

    /// Installs a host-login source even when it is currently signed out.
    /// Resolution failures clear the cache and never fall back to root credentials.
    pub(crate) fn install_host_login_source(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> Result<(), RefreshTokenError> {
        if self.workload_identity_selected {
            return Err(permanent_external_auth_error(
                "workload identity auth cannot be replaced at runtime",
            ));
        }
        {
            let mut source = self.external_auth.write().map_err(|_| {
                RefreshTokenError::Transient(std::io::Error::other(
                    "external auth lock is poisoned",
                ))
            })?;
            *source = Some(external_auth);
        }
        if let Ok(mut cached) = self.inner.write() {
            cached.permanent_refresh_failure = None;
        }
        self.set_cached_auth(/*new_auth*/ None);
        Ok(())
    }

    /// Creates an empty host facade without reading unrelated root credentials.
    pub(crate) async fn shared_host_login_facade_from_auth_config(
        auth_config: AuthConfig,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        let external_auth = WorkloadIdentityExternalAuth::from_process_config(&auth_config)?;
        let mut manager = Self::from_loaded_auth_config(
            auth_config,
            /*enable_codex_api_key_env*/ false,
            /*managed_auth*/ None,
            CredentialSource::Standard,
        );
        manager.workload_identity_selected = external_auth.is_some();
        let manager = Arc::new(manager);
        if let Some(external_auth) = external_auth {
            manager
                .install_external_auth(Arc::new(external_auth))
                .await?;
        }
        Ok(manager)
    }

    /// Creates an OAuth-only manager bound to persisted credentials at this exact home.
    ///
    /// Both initialization and reload ignore environment tokens, process-local auth and workload
    /// identity. The caller must enforce host workload selection before installing an inference
    /// pool; login-method, workspace and network policies still apply to these credentials.
    pub async fn shared_managed_profile_from_auth_config(auth_config: AuthConfig) -> Arc<Self> {
        let auth = auth_config.load_managed_profile_auth().await.ok().flatten();
        Arc::new(Self::from_loaded_auth_config(
            auth_config,
            /*enable_codex_api_key_env*/ false,
            auth,
            CredentialSource::ManagedProfile,
        ))
    }
}

#[cfg(test)]
#[path = "host_login_tests.rs"]
mod tests;
