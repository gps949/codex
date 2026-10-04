//! Local host-source observations for managers; no token hydration or quota request is performed.

use crate::AccountProfileStore;
use crate::AuthConfig;
use crate::AuthDotJson;
use crate::PrimaryLoginSource;
use crate::PrimaryLoginStore;
use crate::primary_login::ready_profile;
use crate::primary_login::stored_owner_hash;
use crate::primary_login::tokens_owner_hash;
use crate::primary_login::validate_stored_mode;
use crate::primary_login_runtime::fingerprint::stored_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use serde::Serialize;

/// Distinguishes local credential availability from authentication resolved by a live host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrimaryLoginStatus {
    StoredReady,
    NeedsLogin,
    RuntimeResolutionRequired,
    Invalid,
    SignedOut,
}

impl PrimaryLoginStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StoredReady => "storedReady",
            Self::NeedsLogin => "needsLogin",
            Self::RuntimeResolutionRequired => "runtimeResolutionRequired",
            Self::Invalid => "invalid",
            Self::SignedOut => "signedOut",
        }
    }
}

/// Nonsecret saved-source identity, independent from observed Remote connection state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryLoginObservation {
    pub source: String,
    pub revision: u64,
    pub profile_id: Option<String>,
    pub label: String,
    pub email: Option<String>,
    pub message: Option<String>,
    pub status: PrimaryLoginStatus,
}

/// Reads the selected source without adopting a different profile or contacting a network.
pub fn observe_primary_login(config: &AuthConfig) -> PrimaryLoginObservation {
    let state = match PrimaryLoginStore::new(config.codex_home.clone()).load() {
        Ok(state) => state,
        Err(error) => {
            return PrimaryLoginObservation {
                source: "invalid".into(),
                revision: 0,
                profile_id: None,
                label: "Host sign-in unavailable".into(),
                email: None,
                message: Some(error.to_string()),
                status: PrimaryLoginStatus::Invalid,
            };
        }
    };
    let mut view = PrimaryLoginObservation {
        source: "root".into(),
        revision: state.revision,
        profile_id: None,
        label: "Root login".into(),
        email: None,
        message: None,
        status: PrimaryLoginStatus::NeedsLogin,
    };
    if crate::is_workload_identity_selected() {
        view.status = PrimaryLoginStatus::RuntimeResolutionRequired;
        view.label = "Host-managed workload identity".into();
        view.message =
            Some("Host-managed workload identity is resolved by the running host.".into());
        return view;
    }
    match state.source {
        PrimaryLoginSource::SignedOut => {
            view.source = "signedOut".into();
            view.label = "Signed out".into();
            view.status = PrimaryLoginStatus::SignedOut;
        }
        PrimaryLoginSource::RootLogin => {
            match config.root_auth_load_is_local() {
                Ok(false) => {
                    view.status = PrimaryLoginStatus::RuntimeResolutionRequired;
                    view.message = Some("External host sign-in is resolved by the running host. Stored root credentials are not its current identity.".into());
                    return view;
                }
                Ok(true) => {}
                Err(error) => {
                    view.message = Some(error.to_string());
                    return view;
                }
            }
            match stored_auth(config, &PrimaryLoginSource::RootLogin) {
                Ok(Some(saved)) => {
                    if root_requires_runtime(&saved) {
                        view.status = PrimaryLoginStatus::RuntimeResolutionRequired;
                        view.message = Some("External host sign-in is resolved by the running host. Stored root credentials are not its current identity.".into());
                    } else if root_has_chatgpt_tokens(&saved) {
                        apply_oauth_observation(&mut view, config, &saved);
                    } else {
                        view.message =
                            Some("Remote Control requires a permitted ChatGPT host login.".into());
                    }
                }
                Ok(None) => {
                    view.message = Some(
                        "Sign in on this host or choose a signed-in subscription profile.".into(),
                    )
                }
                Err(error) => view.message = Some(error.to_string()),
            }
        }
        source @ PrimaryLoginSource::Profile { .. } => {
            let PrimaryLoginSource::Profile {
                profile_id,
                owner_hash,
            } = &source
            else {
                unreachable!()
            };
            view.source = "profile".into();
            view.profile_id = Some(profile_id.to_string());
            view.label = profile_id.to_string();
            let profile = match ready_profile(
                &AccountProfileStore::new(config.codex_home.clone()),
                profile_id,
            ) {
                Ok(profile) => profile,
                Err(error) => {
                    view.message = Some(error.to_string());
                    return view;
                }
            };
            view.label = crate::account_display_name(
                profile.label.as_deref(),
                /*email*/ None,
                profile_id.as_str(),
            )
            .to_string();
            let mut selected = config.clone();
            selected.codex_home = profile.credential_home;
            if selected.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
                view.message = Some("Host profile requires persisted ChatGPT credentials.".into());
                return view;
            }
            let saved = match stored_auth(&selected, &source) {
                Ok(Some(saved)) => saved,
                Ok(None) => {
                    view.message =
                        Some("Selected profile has no stored ChatGPT credentials.".into());
                    return view;
                }
                Err(error) => {
                    view.message = Some(error.to_string());
                    return view;
                }
            };
            if let Err(error) = validate_stored_mode(&saved) {
                view.message = Some(error.to_string());
                return view;
            }
            if saved
                .tokens
                .as_ref()
                .and_then(|tokens| tokens_owner_hash(tokens).ok())
                .as_ref()
                != Some(owner_hash)
            {
                view.message = Some(
                    "Selected profile identity changed. Select this host account again.".into(),
                );
                return view;
            }
            match stored_owner_hash(&selected) {
                Ok(current) if &current == owner_hash => {
                    apply_oauth_observation(&mut view, &selected, &saved);
                    // The email is only derived after the persisted source owner is validated.
                    view.label = crate::account_display_name(
                        profile.label.as_deref(),
                        view.email.as_deref(),
                        profile_id.as_str(),
                    )
                    .to_string();
                }
                Ok(_) => {
                    view.message = Some(
                        "Selected profile identity changed. Select this host account again.".into(),
                    )
                }
                Err(error) => view.message = Some(error.to_string()),
            }
        }
    }
    view
}

fn root_requires_runtime(saved: &AuthDotJson) -> bool {
    matches!(
        saved.auth_mode,
        Some(AuthMode::PersonalAccessToken | AuthMode::AgentIdentity | AuthMode::Headers)
    ) || (saved.auth_mode.is_none()
        && (saved.personal_access_token.is_some() || saved.agent_identity.is_some()))
}

fn root_has_chatgpt_tokens(saved: &AuthDotJson) -> bool {
    match saved.auth_mode {
        Some(AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens) => true,
        None => {
            saved.openai_api_key.is_none()
                && saved.bedrock_api_key.is_none()
                && saved.bedrock_access_keys.is_none()
        }
        Some(
            AuthMode::ApiKey
            | AuthMode::Headers
            | AuthMode::AgentIdentity
            | AuthMode::PersonalAccessToken
            | AuthMode::BedrockApiKey
            | AuthMode::BedrockAccessKeys,
        ) => false,
    }
}

fn apply_oauth_observation(
    view: &mut PrimaryLoginObservation,
    config: &AuthConfig,
    saved: &AuthDotJson,
) {
    let Some(tokens) = saved.tokens.as_ref().filter(|tokens| {
        !tokens.access_token.trim().is_empty() && tokens_owner_hash(tokens).is_ok()
    }) else {
        view.message =
            Some("Stored ChatGPT login is incomplete. Complete host sign-in again.".into());
        return;
    };
    if !config.is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        || config
            .effective_chatgpt_workspaces()
            .is_some_and(|workspaces| {
                tokens
                    .account_id
                    .as_ref()
                    .is_none_or(|workspace| !workspaces.contains(workspace))
            })
    {
        view.message = Some("The authentication policy does not permit this host account.".into());
        return;
    }
    view.email = tokens.id_token.email.clone();
    view.status = PrimaryLoginStatus::StoredReady;
}
