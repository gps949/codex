use base64::Engine;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;
use crate::AccountProfileId;
use crate::auth::AuthDotJson;
use crate::auth::save_auth;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;

fn credentials(user: &str, workspace: &str, refresh: &str) -> AuthDotJson {
    let payload = serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_user_id": user,
            "chatgpt_account_id": workspace
        }
    });
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).unwrap());
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&format!("e30.{encoded}.sig")).unwrap(),
            access_token: "test-access".into(),
            refresh_token: refresh.into(),
            account_id: Some(workspace.into()),
        }),
        last_refresh: Some(chrono::Utc::now()),
        openai_api_key: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

#[test]
fn relogin_preserves_the_original_seat_when_browser_selects_a_different_workspace() {
    let home = TempDir::new().unwrap();
    let profile = AccountProfile::new(
        AccountProfileId::new("personal").unwrap(),
        home.path().join("personal"),
        /*priority*/ 0,
        Some("Personal".into()),
    );
    let original = credentials("same-user", "personal", "old-refresh");
    let mode = AuthCredentialsStoreMode::File;
    let backend = AuthKeyringBackendKind::default();
    save_auth(&profile.credential_home, &original, mode, backend).unwrap();
    let staging = ReloginStaging::new(home.path(), &profile, mode, backend).unwrap();
    save_auth(
        staging.home(),
        &credentials("same-user", "business", "new-refresh"),
        mode,
        backend,
    )
    .unwrap();

    assert!(staging.commit(&profile, mode, backend).is_err());
    assert_eq!(
        load_auth_dot_json(&profile.credential_home, mode, backend).unwrap(),
        Some(original)
    );
}

#[test]
fn relogin_commits_the_same_seat_and_notifies_running_pools() {
    let home = TempDir::new().unwrap();
    let profile = AccountProfile::new(
        AccountProfileId::new("work").unwrap(),
        home.path().join("work"),
        /*priority*/ 0,
        Some("Work".into()),
    );
    let mode = AuthCredentialsStoreMode::File;
    let backend = AuthKeyringBackendKind::default();
    save_auth(
        &profile.credential_home,
        &credentials("user", "workspace", "old"),
        mode,
        backend,
    )
    .unwrap();
    let staging = ReloginStaging::new(home.path(), &profile, mode, backend).unwrap();
    let repaired = credentials("user", "workspace", "new");
    save_auth(staging.home(), &repaired, mode, backend).unwrap();

    staging.commit(&profile, mode, backend).unwrap();
    assert_eq!(
        load_auth_dot_json(&profile.credential_home, mode, backend).unwrap(),
        Some(repaired)
    );
    assert!(crate::account_credentials::credential_version(&profile.credential_home).is_some());
}
