use codex_protocol::account::PlanType;
use codex_protocol::auth::KnownPlan;
use codex_protocol::auth::PlanType as AuthPlanType;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::RateLimitReachedType;
use codex_protocol::protocol::RateLimitSnapshot;

use super::plans_share_quota_bucket;
use super::usage_limit_matches_profile;

#[test]
fn workspace_and_consumer_plans_do_not_share_quota_bucket() {
    assert!(!plans_share_quota_bucket(PlanType::Team, PlanType::Plus));
    assert!(!plans_share_quota_bucket(PlanType::Business, PlanType::Pro));
}

#[test]
fn workspace_plans_share_quota_bucket() {
    assert!(plans_share_quota_bucket(PlanType::Team, PlanType::Business));
}

#[test]
fn consumer_plans_share_quota_bucket() {
    assert!(plans_share_quota_bucket(PlanType::Plus, PlanType::Pro));
}

#[tokio::test]
async fn workspace_member_limit_does_not_match_plus_profile() {
    let (_home, lease) = test_lease_with_plan(PlanType::Plus).await;
    let limit = UsageLimitReachedError {
        plan_type: Some(AuthPlanType::Known(KnownPlan::Team)),
        resets_at: None,
        rate_limits: None,
        promo_message: None,
        rate_limit_reached_type: Some(RateLimitReachedType::WorkspaceMemberUsageLimitReached),
    };
    assert!(!usage_limit_matches_profile(&lease, &limit).await);
}

#[tokio::test]
async fn matching_team_limit_matches_team_profile() {
    let (_home, lease) = test_lease_with_plan(PlanType::Team).await;
    let limit = UsageLimitReachedError {
        plan_type: Some(AuthPlanType::Known(KnownPlan::Team)),
        resets_at: None,
        rate_limits: None,
        promo_message: None,
        rate_limit_reached_type: Some(RateLimitReachedType::WorkspaceMemberUsageLimitReached),
    };
    assert!(usage_limit_matches_profile(&lease, &limit).await);
}

#[tokio::test]
async fn mismatched_plan_type_in_rate_limits_snapshot_is_rejected() {
    let (_home, lease) = test_lease_with_plan(PlanType::Plus).await;
    let limit = UsageLimitReachedError {
        plan_type: None,
        resets_at: None,
        rate_limits: Some(Box::new(RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            plan_type: Some(PlanType::Business),
            primary: None,
            secondary: None,
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            rate_limit_reached_type: None,
        })),
        promo_message: None,
        rate_limit_reached_type: None,
    };
    assert!(!usage_limit_matches_profile(&lease, &limit).await);
}

async fn test_lease_with_plan(plan: PlanType) -> (tempfile::TempDir, codex_login::AccountLease) {
    use base64::Engine as _;
    use codex_config::ManagedAuthPolicy;
    use codex_login::AccountPool;
    use codex_login::AccountProfile;
    use codex_login::AccountProfileId;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthManager;
    use codex_login::auth::AuthKeyringBackendKind;
    use std::sync::Arc;

    let temp = tempfile::TempDir::new().expect("temp dir");
    let profile_id = AccountProfileId::new("profile-a").expect("profile id");
    let credential_home = temp.path().join("auth-profiles").join("profile-a");
    std::fs::create_dir_all(&credential_home).expect("credential home");

    let plan_json = match plan {
        PlanType::Plus => "plus",
        PlanType::Team => "team",
        PlanType::Business => "business",
        other => panic!("unsupported test plan: {other:?}"),
    };
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header =
        b64(&serde_json::to_vec(&serde_json::json!({ "alg": "none", "typ": "JWT" })).unwrap());
    let payload = b64(&serde_json::to_vec(&serde_json::json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": plan_json,
            "chatgpt_account_id": "workspace-1",
        },
        "chatgpt_user_id": "user-plus",
    }))
    .unwrap());
    let fake_jwt = format!("{header}.{payload}.{}", b64(b"sig"));
    std::fs::write(
        credential_home.join("auth.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "account_id": "workspace-1",
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .expect("write auth"),
    )
    .expect("write auth.json");

    let auth_config = codex_login::AuthConfig {
        codex_home: credential_home.clone(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method: None,
        chatgpt_base_url: None,
        forced_chatgpt_workspace_id: None,
        managed_auth_policy: ManagedAuthPolicy::default(),
        auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
    };
    let manager =
        AuthManager::shared_from_auth_config(auth_config, /*enable_codex_api_key_env*/ false)
            .await
            .expect("auth manager");
    let pool = Arc::new(AccountPool::new());
    let profile = AccountProfile::new(profile_id, credential_home, 0, Some("test".to_string()));
    pool.register(profile, manager).expect("register profile");
    let lease = pool.lease().expect("lease");
    (temp, lease)
}
