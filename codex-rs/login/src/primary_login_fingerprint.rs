//! Local credential observations preserve external root-token precedence.

use crate::AuthConfig;
use crate::AuthDotJson;
use crate::PrimaryLoginSource;
use sha2::Digest;
use sha2::Sha256;
use std::io;

pub(crate) fn stored_auth(
    config: &AuthConfig,
    source: &PrimaryLoginSource,
) -> io::Result<Option<AuthDotJson>> {
    if *source == PrimaryLoginSource::RootLogin
        && let Some(auth) = crate::auth::load_auth_dot_json(
            &config.codex_home,
            codex_config::types::AuthCredentialsStoreMode::Ephemeral,
            config.keyring_backend_kind,
        )?
    {
        return Ok(Some(auth));
    }
    crate::auth::load_auth_dot_json(
        &config.codex_home,
        config.auth_credentials_store_mode,
        config.keyring_backend_kind,
    )
}

pub(super) fn storage_fingerprint(
    config: &AuthConfig,
    source: &PrimaryLoginSource,
) -> io::Result<Option<[u8; 32]>> {
    fingerprint_auth(stored_auth(config, source)?.as_ref())
}

pub(super) fn fingerprint_auth(auth: Option<&AuthDotJson>) -> io::Result<Option<[u8; 32]>> {
    auth.map(|auth| {
        serde_json::to_vec(auth)
            .map(|bytes| Sha256::digest(bytes).into())
            .map_err(io::Error::other)
    })
    .transpose()
}
