use std::fs;
use std::path::PathBuf;

use base64::Engine;
use chrono::Utc;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::PrimaryLoginSource;
use super::PrimaryLoginState;
use super::PrimaryLoginStore;
use crate::AccountProfile;
use crate::AccountProfileId;
use crate::AccountProfileMetadataUpdate;
use crate::AccountProfileStore;
use crate::AuthConfig;
use crate::AuthDotJson;
use crate::AuthKeyringBackendKind;
use crate::TokenData;
use crate::save_auth;

pub(crate) fn config(codex_home: PathBuf) -> AuthConfig {
    AuthConfig {
        codex_home,
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_login_method: None,
        chatgpt_base_url: None,
        forced_chatgpt_workspace_id: None,
        managed_auth_policy: ManagedAuthPolicy::default(),
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    }
}

pub(crate) fn credentials(user: &str, workspace: &str, suffix: &str) -> AuthDotJson {
    let payload = serde_json::json!({
        "email": format!("{user}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_user_id": user, "chatgpt_account_id": workspace
        }
    });
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    let jwt = format!("e30.{payload}.sig");
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: crate::token_data::parse_chatgpt_jwt_claims(&jwt).unwrap(),
            access_token: format!("access-{suffix}"),
            refresh_token: format!("refresh-{suffix}"),
            account_id: Some(workspace.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

pub(crate) fn write_auth(home: &std::path::Path, auth: &AuthDotJson) {
    save_auth(
        home,
        auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
}

pub(crate) fn ready_profile(home: &std::path::Path, user: &str, workspace: &str) -> AccountProfile {
    let profiles = AccountProfileStore::new(home.to_path_buf());
    let profile = profiles.allocate_profile(None, /*priority*/ 10).unwrap();
    write_auth(
        &profile.credential_home,
        &credentials(user, workspace, user),
    );
    profiles.complete_profile(&profile.id).unwrap()
}

#[test]
fn missing_source_keeps_stock_root_login_without_materializing_metadata() {
    let home = TempDir::new().unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    assert_eq!(store.load().unwrap(), PrimaryLoginState::default());
    assert!(!store.path().exists());
}

#[test]
fn explicit_sign_out_and_root_selection_are_persistent_revisioned_choices() {
    let home = TempDir::new().unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let signed_out = store.sign_out().unwrap();
    assert_eq!(signed_out.source, PrimaryLoginSource::SignedOut);
    assert_eq!(store.load().unwrap(), signed_out);
    let root = store.use_root().unwrap();
    assert_eq!(root.source, PrimaryLoginSource::RootLogin);
    assert_eq!(root.revision, signed_out.revision + 1);
}

#[tokio::test]
async fn primary_selection_does_not_copy_credentials_or_change_inference_selection() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "user-a", "workspace-a");
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let before_manifest = fs::read(profiles.manifest_path()).unwrap();
    let runtime_path = home.path().join("account-runtime-state.json");
    fs::write(&runtime_path, "unchanged inference state").unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let selected = store
        .select_profile(&config(home.path().to_path_buf()), &profile.id)
        .await
        .unwrap();
    assert!(
        matches!(&selected.source, PrimaryLoginSource::Profile { profile_id, .. } if profile_id == &profile.id)
    );
    assert_eq!(fs::read(profiles.manifest_path()).unwrap(), before_manifest);
    assert_eq!(
        fs::read_to_string(runtime_path).unwrap(),
        "unchanged inference state"
    );
    assert!(!home.path().join("auth.json").exists());
    assert!(PrimaryLoginStore::is_profile_selected(home.path(), &profile.id).unwrap());
    let persisted = fs::read_to_string(store.path()).unwrap();
    for sensitive in [
        "user-a",
        "workspace-a",
        "@example.com",
        "access-user-a",
        "refresh-user-a",
    ] {
        assert!(
            !persisted.contains(sensitive),
            "source metadata leaked {sensitive}"
        );
    }
}

#[tokio::test]
async fn disabled_profile_can_be_used_for_host_login() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "user-a", "workspace-a");
    AccountProfileStore::new(home.path().to_path_buf())
        .update_profile_metadata(
            &profile.id,
            AccountProfileMetadataUpdate {
                disabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&config(home.path().to_path_buf()), &profile.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn two_seats_in_one_workspace_have_distinct_primary_owner_bindings() {
    let home = TempDir::new().unwrap();
    let first = ready_profile(home.path(), "seat-a", "workspace");
    let second = ready_profile(home.path(), "seat-b", "workspace");
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let first = store
        .select_profile(&config(home.path().to_path_buf()), &first.id)
        .await
        .unwrap();
    let second = store
        .select_profile(&config(home.path().to_path_buf()), &second.id)
        .await
        .unwrap();
    let (
        PrimaryLoginSource::Profile {
            owner_hash: first, ..
        },
        PrimaryLoginSource::Profile {
            owner_hash: second, ..
        },
    ) = (first.source, second.source)
    else {
        panic!("expected profile sources");
    };
    assert_ne!(first, second);
}

#[tokio::test]
async fn incomplete_and_non_subscription_stored_auth_cannot_replace_primary_login() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "user-a", "workspace-a");
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    store
        .select_profile(&config(home.path().to_path_buf()), &profile.id)
        .await
        .unwrap();
    let before = fs::read(store.path()).unwrap();
    let mut incomplete = credentials("user-a", "workspace-a", "incomplete");
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"https://api.openai.com/auth": {"chatgpt_account_id": "workspace-a"}})).unwrap()
    );
    let tokens = incomplete.tokens.as_mut().unwrap();
    tokens.id_token.chatgpt_user_id = None;
    tokens.id_token.raw_jwt = format!("e30.{payload}.synthetic");
    for invalid in [
        incomplete,
        AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("synthetic-key".into()),
            ..credentials("user-a", "workspace-a", "api")
        },
        AuthDotJson {
            auth_mode: Some(AuthMode::PersonalAccessToken),
            personal_access_token: Some("synthetic-pat".into()),
            ..credentials("user-a", "workspace-a", "pat")
        },
    ] {
        write_auth(&profile.credential_home, &invalid);
        assert!(
            store
                .select_profile(&config(home.path().to_path_buf()), &profile.id)
                .await
                .is_err()
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
    }
}

#[tokio::test]
async fn pending_or_removed_profile_cannot_replace_primary_login() {
    let home = TempDir::new().unwrap();
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let pending = profiles.allocate_profile(None, /*priority*/ 10).unwrap();
    write_auth(
        &pending.credential_home,
        &credentials("user-a", "workspace-a", "pending"),
    );
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    assert!(
        store
            .select_profile(&config(home.path().to_path_buf()), &pending.id)
            .await
            .is_err()
    );
    profiles.complete_profile(&pending.id).unwrap();
    profiles.remove_profile_metadata(&pending.id).unwrap();
    assert!(
        store
            .select_profile(&config(home.path().to_path_buf()), &pending.id)
            .await
            .is_err()
    );
    assert!(!store.path().exists());
}

#[tokio::test]
async fn primary_profile_obeys_current_login_and_workspace_restrictions() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "user-a", "workspace-a");
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let mut cfg = config(home.path().to_path_buf());
    cfg.forced_chatgpt_workspace_id = Some(vec!["other-workspace".into()]);
    assert!(store.select_profile(&cfg, &profile.id).await.is_err());
    cfg.forced_chatgpt_workspace_id = None;
    cfg.forced_login_method = Some(ForcedLoginMethod::Api);
    assert!(store.select_profile(&cfg, &profile.id).await.is_err());
    assert!(!store.path().exists());
}

#[test]
fn malformed_unknown_and_oversized_sources_fail_closed() {
    let home = TempDir::new().unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    for invalid in ["{".to_string(), r#"{"version":2,"revision":1,"source":{"type":"root_login"}}"#.to_string(), r#"{"version":1,"revision":1,"source":{"type":"profile","profile_id":"../escape","owner_hash":"abc"}}"#.to_string(), " ".repeat(65537)] {
        fs::write(store.path(), invalid).unwrap();
        assert!(store.load().is_err());
        assert!(PrimaryLoginStore::is_profile_selected(home.path(), &AccountProfileId::new("other").unwrap()).is_err());
    }
}

#[test]
fn revision_overflow_keeps_existing_choice_intact() {
    let home = TempDir::new().unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let state = PrimaryLoginState {
        version: 1,
        revision: u64::MAX,
        source: PrimaryLoginSource::SignedOut,
    };
    let original = serde_json::to_vec(&state).unwrap();
    fs::write(store.path(), &original).unwrap();
    assert!(store.use_root().is_err());
    assert_eq!(fs::read(store.path()).unwrap(), original);
}

#[tokio::test]
async fn selected_host_profile_refuses_metadata_and_credential_deletion_until_signed_out() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "user-a", "workspace-a");
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let before_manifest = fs::read(profiles.manifest_path()).unwrap();
    let before_credentials = fs::read(profile.credential_home.join("auth.json")).unwrap();
    store
        .select_profile(&config(home.path().to_path_buf()), &profile.id)
        .await
        .unwrap();
    assert!(profiles.remove_profile_metadata(&profile.id).is_err());
    assert!(profiles.purge_managed_credentials(&profile.id).is_err());
    assert_eq!(
        (
            fs::read(profiles.manifest_path()).unwrap(),
            fs::read(profile.credential_home.join("auth.json")).unwrap()
        ),
        (before_manifest, before_credentials)
    );
    store.sign_out().unwrap();
    assert_eq!(profiles.remove_profile_metadata(&profile.id).unwrap(), true);
    assert_eq!(
        profiles.purge_managed_credentials(&profile.id).unwrap(),
        true
    );
}
