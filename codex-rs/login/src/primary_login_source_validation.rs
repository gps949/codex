//! Local source and owner rechecks shared by observation and request resolution.

use super::*;

impl PrimaryLoginResolver {
    pub(super) fn cached_owner_is_current(&self, auth: Option<&CodexAuth>) -> io::Result<bool> {
        let state = self.store.load()?;
        match state.source {
            PrimaryLoginSource::SignedOut => Ok(false),
            PrimaryLoginSource::Profile {
                profile_id,
                owner_hash,
            } => {
                let profile = ready_profile(
                    &AccountProfileStore::new(self.config.codex_home.clone()),
                    &profile_id,
                )?;
                let mut config = self.config.clone();
                config.codex_home = profile.credential_home;
                if stored_owner_hash(&config)? != owner_hash {
                    return Err(source_changed());
                }
                Ok(auth.is_some_and(|auth| {
                    managed_owner_hash(auth).is_ok_and(|current| current == owner_hash)
                }))
            }
            PrimaryLoginSource::RootLogin => {
                let Some(auth) = auth else {
                    return Ok(true);
                };
                if matches!(
                    auth,
                    CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_)
                ) {
                    let stored = stored_auth(&self.config, &PrimaryLoginSource::RootLogin)?;
                    let tokens = stored
                        .as_ref()
                        .and_then(|auth| auth.tokens.as_ref())
                        .ok_or_else(source_changed)?;
                    return Ok(crate::primary_login::tokens_owner_hash(tokens)?
                        == crate::primary_login::tokens_owner_hash(&auth.get_token_data()?)?);
                }
                // Hydrating a new root PAT can await HTTP. Retire the old cached identity before
                // that wait while retaining stock environment-token precedence in the resolver.
                let stored = stored_auth(&self.config, &PrimaryLoginSource::RootLogin)?;
                if auth.is_personal_access_token_auth()
                    && let Some(previous) = stored
                        .as_ref()
                        .and_then(|stored| stored.personal_access_token.as_ref())
                {
                    return Ok(auth.get_token().is_ok_and(|current| &current == previous));
                }
                Ok(true)
            }
        }
    }

    pub(super) fn verify_source(
        &self,
        expected: &PrimaryLoginState,
        config: &AuthConfig,
    ) -> io::Result<()> {
        if self.store.load()? != *expected {
            return Err(source_changed());
        }
        if let PrimaryLoginSource::Profile {
            profile_id,
            owner_hash,
        } = &expected.source
        {
            let profile = ready_profile(
                &AccountProfileStore::new(self.config.codex_home.clone()),
                profile_id,
            )?;
            if profile.credential_home != config.codex_home
                || stored_owner_hash(config)? != *owner_hash
            {
                return Err(source_changed());
            }
        }
        Ok(())
    }
}
