//! Independent, endpoint-bound credentials, outside every inference account inventory.

use crate::DecisionAdvisorCredentialSource;
use crate::DecisionAdvisorMode;
use crate::DecisionAdvisorSettings;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// Request-owned secret: deliberately has no serialization implementation.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct DecisionAdvisorSecret(String);

impl fmt::Debug for DecisionAdvisorSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl From<&str> for DecisionAdvisorSecret {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

impl From<String> for DecisionAdvisorSecret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl DecisionAdvisorSecret {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.0.trim().is_empty() || self.0.len() > 16_384 || self.0.contains(['\r', '\n']) {
            return Err(io::Error::other("Invalid decision service key"));
        }
        Ok(())
    }
}

pub struct DecisionAdvisorCredentialStore {
    home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring: AuthKeyringBackendKind,
}

impl DecisionAdvisorCredentialStore {
    pub fn new(
        home: PathBuf,
        mode: AuthCredentialsStoreMode,
        keyring: AuthKeyringBackendKind,
    ) -> Self {
        Self {
            home,
            mode,
            keyring,
        }
    }

    fn credential_home(&self, settings: &DecisionAdvisorSettings) -> io::Result<PathBuf> {
        let mut active = settings.clone();
        active.mode = DecisionAdvisorMode::Shadow;
        active.validate().map_err(io::Error::other)?;
        let endpoint = url::Url::parse(&settings.endpoint).map_err(io::Error::other)?;
        let mut digest = Sha256::new();
        digest.update(b"codex-decision-credential-v1\0");
        digest.update(serde_json::to_vec(&settings.provider).map_err(io::Error::other)?);
        digest.update(b"\0");
        digest.update(endpoint.as_str().as_bytes());
        Ok(self
            .home
            .join("decision-auth-profiles")
            .join(format!("{:x}", digest.finalize())))
    }

    pub fn load(
        &self,
        settings: &DecisionAdvisorSettings,
    ) -> io::Result<Option<DecisionAdvisorSecret>> {
        let home = self.credential_home(settings)?;
        codex_login::load_auth_dot_json(&home, self.mode, self.keyring)
            .map_err(|_| io::Error::other("Decision credential storage is unavailable"))
            .map(|auth| {
                auth.and_then(|auth| auth.openai_api_key)
                    .map(DecisionAdvisorSecret::from)
            })
    }

    pub fn resolve(
        &self,
        settings: &DecisionAdvisorSettings,
        environment: impl FnOnce(&str) -> Option<String>,
    ) -> io::Result<Option<DecisionAdvisorSecret>> {
        match settings.credential_source {
            DecisionAdvisorCredentialSource::Environment => Ok((!settings.api_key_env.is_empty())
                .then(|| environment(&settings.api_key_env))
                .flatten()
                .filter(|key| !key.trim().is_empty())
                .map(DecisionAdvisorSecret::from)),
            DecisionAdvisorCredentialSource::Stored => self.load(settings),
        }
    }

    pub fn save(
        &self,
        settings: &DecisionAdvisorSettings,
        secret: &DecisionAdvisorSecret,
    ) -> io::Result<()> {
        let key = secret.expose_secret();
        secret.validate()?;
        let home = self.credential_home(settings)?;
        std::fs::create_dir_all(&home)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                home.parent()
                    .ok_or_else(|| io::Error::other("Invalid decision credential location"))?,
                std::fs::Permissions::from_mode(0o700),
            )?;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
        }
        let auth = AuthDotJson {
            auth_mode: None,
            openai_api_key: Some(key.into()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };
        codex_login::save_auth(&home, &auth, self.mode, self.keyring)
            .map_err(|_| io::Error::other("Decision credential storage is unavailable"))
    }

    pub fn remove(&self, settings: &DecisionAdvisorSettings) -> io::Result<()> {
        codex_login::logout(&self.credential_home(settings)?, self.mode, self.keyring)
            .map_err(|_| io::Error::other("Decision credential storage is unavailable"))
            .map(|_| ())
    }
}

#[cfg(test)]
#[path = "decision_advisor_credentials_tests.rs"]
mod tests;
