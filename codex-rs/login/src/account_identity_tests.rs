use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::auth::AuthDotJson;
use crate::auth::AuthKeyringBackendKind;
use crate::auth::save_auth;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use base64::Engine;

fn chatgpt_auth_with_ids(
    chatgpt_user_id: &str,
    chatgpt_account_id: &str,
    email: &str,
) -> AuthDotJson {
    #[derive(serde::Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": chatgpt_user_id,
            "user_id": chatgpt_user_id,
            "chatgpt_account_id": chatgpt_account_id
        }
    });
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = b64(&serde_json::to_vec(&header).expect("header should serialize"));
    let payload_b64 = b64(&serde_json::to_vec(&payload).expect("payload should serialize"));
    let fake_jwt = format!("{header_b64}.{payload_b64}.sig");

    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&fake_jwt).expect("fake jwt should parse"),
            access_token: format!("{chatgpt_user_id}-access"),
            refresh_token: format!("{chatgpt_user_id}-refresh"),
            account_id: Some(chatgpt_account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    }
}

#[test]
fn duplicate_login_matches_chatgpt_user_id_not_workspace_account_id() {
    let temp = TempDir::new().expect("temp dir");
    let store = AccountProfileStore::new(temp.path().join("codex"));
    let workspace_id = "workspace-shared-123";

    store
        .ensure_legacy_root_profile(Some("root".to_string()), 0)
        .expect("legacy profile");
    let pending = store
        .allocate_profile(Some("ceo".to_string()), 20)
        .expect("allocate profile");

    let root_auth = chatgpt_auth_with_ids("user-admin", workspace_id, "admin@example.com");
    let ceo_auth = chatgpt_auth_with_ids("user-ceo", workspace_id, "ceo@example.com");
    save_auth(
        store.codex_home(),
        &root_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save root auth");
    save_auth(
        &pending.credential_home,
        &ceo_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save ceo auth");

    let _ceo_identity = load_login_identity(
        &pending.credential_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load ceo identity")
    .expect("ceo identity");

    assert_eq!(
        reconcile_duplicate_new_login(
            &store,
            &pending,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("reconcile"),
        None
    );

    save_auth(
        &pending.credential_home,
        &root_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save duplicate admin auth");

    let existing = reconcile_duplicate_new_login(
        &store,
        &pending,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("reconcile duplicate")
    .expect("existing profile");
    assert_eq!(existing.id.as_str(), "legacy-root");
    assert_eq!(
        load_login_identity(
            existing.credential_home.as_path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("reload root identity")
        .expect("root identity"),
        login_identity_from_auth(&root_auth).expect("root auth identity")
    );
    assert!(
        store
            .load_profile_records()
            .expect("profiles")
            .iter()
            .all(|record| record.profile.id != pending.id)
    );
}
