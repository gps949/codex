//! Reports and selects host sign-in independently from inference scheduling.

use super::AccountManager;
use codex_login::PrimaryLoginSource;
use codex_login::PrimaryLoginStore;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryLoginView {
    pub source: String,
    pub profile_id: Option<String>,
    pub label: String,
    pub email: Option<String>,
    pub ready: bool,
    pub message: Option<String>,
}

impl AccountManager {
    pub(super) fn primary_login_view(&self) -> PrimaryLoginView {
        let store = PrimaryLoginStore::new(self.config.codex_home.to_path_buf());
        let state = match store.load() {
            Ok(state) => state,
            Err(error) => {
                return PrimaryLoginView {
                    source: "invalid".into(),
                    profile_id: None,
                    label: "Host sign-in unavailable".into(),
                    email: None,
                    ready: false,
                    message: Some(error.to_string()),
                };
            }
        };
        let (source, profile_id, label, home, expected_owner) = match state.source {
            PrimaryLoginSource::RootLogin => (
                "root",
                None,
                "Root login".into(),
                self.config.codex_home.to_path_buf(),
                None,
            ),
            PrimaryLoginSource::Profile {
                profile_id,
                owner_hash,
            } => match self.profile(profile_id.as_str()) {
                Ok(profile) if profile.state == codex_login::AccountProfileState::Ready => {
                    let label = profile
                        .profile
                        .label
                        .clone()
                        .unwrap_or_else(|| profile_id.to_string());
                    (
                        "profile",
                        Some(profile_id.to_string()),
                        label,
                        profile.profile.credential_home,
                        Some(owner_hash),
                    )
                }
                Ok(_) => {
                    return PrimaryLoginView {
                        source: "profile".into(),
                        profile_id: Some(profile_id.to_string()),
                        label: "Selected account unavailable".into(),
                        email: None,
                        ready: false,
                        message: Some(
                            "Complete sign-in for the selected subscription account.".into(),
                        ),
                    };
                }
                Err(error) => {
                    return PrimaryLoginView {
                        source: "profile".into(),
                        profile_id: Some(profile_id.to_string()),
                        label: "Selected account unavailable".into(),
                        email: None,
                        ready: false,
                        message: Some(error.to_string()),
                    };
                }
            },
            PrimaryLoginSource::SignedOut => {
                return PrimaryLoginView {
                    source: "signedOut".into(),
                    profile_id: None,
                    label: "Signed out".into(),
                    email: None,
                    ready: false,
                    message: None,
                };
            }
        };
        let auth = codex_login::load_auth_dot_json(
            &home,
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
        )
        .ok()
        .flatten();
        let email = auth
            .as_ref()
            .and_then(|auth| auth.tokens.as_ref())
            .and_then(|tokens| tokens.id_token.email.clone());
        let mut ready = auth.as_ref().is_some_and(|auth| {
            (auth.auth_mode == Some(codex_protocol::auth::AuthMode::Chatgpt)
                || (auth.auth_mode.is_none()
                    && auth.openai_api_key.is_none()
                    && auth.personal_access_token.is_none()
                    && auth.agent_identity.is_none()
                    && auth.bedrock_api_key.is_none()
                    && auth.bedrock_access_keys.is_none()))
                && auth.tokens.as_ref().is_some_and(|tokens| {
                    !tokens.access_token.is_empty()
                        && tokens.account_id.as_ref().is_some_and(|id| !id.is_empty())
                })
        });
        if let Some(expected) = expected_owner {
            let identity = auth
                .as_ref()
                .and_then(|auth| auth.tokens.as_ref())
                .and_then(|tokens| {
                    Some((
                        tokens.account_id.as_deref()?,
                        tokens.id_token.chatgpt_user_id.as_deref()?,
                    ))
                });
            ready &= identity.is_some_and(|(account, user)| {
                let mut digest = Sha256::new();
                digest.update(b"codex-primary-login-owner-v1\0");
                for component in [user, account] {
                    digest.update((component.len() as u64).to_be_bytes());
                    digest.update(component.as_bytes());
                }
                format!("{:x}", digest.finalize()) == expected
            });
        }
        let label = if source == "profile" {
            let raw = profile_id
                .as_deref()
                .and_then(|id| self.profile(id).ok())
                .and_then(|record| record.profile.label);
            codex_login::account_display_name(raw.as_deref(), email.as_deref(), &label).to_string()
        } else {
            label
        };
        PrimaryLoginView {
            source: source.into(),
            profile_id,
            label,
            email,
            ready,
            message: None,
        }
    }
}
