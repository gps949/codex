use chrono::Duration;
use chrono::Utc;
use codex_config::AutoResetCredits;
use codex_login::AccountPool;
use codex_login::AccountProfile;
use codex_login::AccountProfileId;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::consume_reset_credit_for_profile;
use super::reactivate_redeemed_profile;
use super::should_redeem;
use crate::config::ConfigBuilder;

#[test]
fn never_mode_never_redeems() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::Never,
            Duration::minutes(60),
            /*earliest_reset*/ None,
            now
        ),
        false
    );
}

#[test]
fn nearby_natural_reset_wins_over_a_credit() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            Some(now + Duration::minutes(30)),
            now
        ),
        false
    );
}

#[test]
fn distant_reset_justifies_redeeming() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            Some(now + Duration::hours(4)),
            now
        ),
        true
    );
}

#[test]
fn unknown_reset_justifies_redeeming() {
    let now = Utc::now();
    assert_eq!(
        should_redeem(
            AutoResetCredits::WhenPoolExhausted,
            Duration::minutes(60),
            /*earliest_reset*/ None,
            now
        ),
        true
    );
}

#[test]
fn redeemed_credit_does_not_report_success_when_profile_cannot_reactivate() {
    let pool = AccountPool::new();
    let profile_id = AccountProfileId::new("disabled-after-consume").expect("valid profile id");
    pool.register(
        AccountProfile::new(
            profile_id.clone(),
            std::path::PathBuf::from("/tmp/disabled-after-consume"),
            0,
            /*label*/ None,
        ),
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
    )
    .expect("register profile");
    pool.set_disabled(&profile_id, true)
        .expect("disable profile after consume");

    assert!(reactivate_redeemed_profile(&pool, profile_id).is_none());
}

#[tokio::test]
async fn redemption_uses_the_failed_profile_auth_on_the_real_backend_route() -> anyhow::Result<()> {
    const PRIMARY_TOKEN: &str = "e30.e30.cHJpbWFyeQ";
    const FAILED_TOKEN: &str = "e30.e30.ZmFpbGVk";

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());

    let pool = AccountPool::new();
    let primary_id = AccountProfileId::new("primary-profile")?;
    let failed_id = AccountProfileId::new("failed-profile")?;
    pool.register(
        AccountProfile::new(
            primary_id,
            std::path::PathBuf::from("primary-profile"),
            /*priority*/ 0,
            /*label*/ None,
        ),
        AuthManager::from_auth_for_testing(CodexAuth::from_external_chatgpt_tokens(
            PRIMARY_TOKEN,
            "account-primary",
            Some("pro"),
        )?),
    )?;
    pool.register(
        AccountProfile::new(
            failed_id.clone(),
            std::path::PathBuf::from("failed-profile"),
            /*priority*/ 10,
            /*label*/ None,
        ),
        AuthManager::from_auth_for_testing(CodexAuth::from_external_chatgpt_tokens(
            FAILED_TOKEN,
            "account-failed",
            Some("pro"),
        )?),
    )?;

    assert!(
        consume_reset_credit_for_profile(&pool, &failed_id, &config, "stable-request-id").await
    );
    let auth_headers = server
        .received_requests()
        .await
        .expect("captured reset-credit request")
        .into_iter()
        .map(|request| {
            (
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                request
                    .headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        auth_headers,
        vec![(
            Some(format!("Bearer {FAILED_TOKEN}")),
            Some("account-failed".to_string()),
        )]
    );
    Ok(())
}
