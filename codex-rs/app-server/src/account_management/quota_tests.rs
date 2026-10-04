use super::*;
use base64::Engine as _;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct Fixture {
    home: tempfile::TempDir,
    server: MockServer,
    manager: Arc<AccountManager>,
    id: codex_login::AccountProfileId,
    reset: DateTime<Utc>,
    cached: codex_login::AccountRuntimeProfileState,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let server = MockServer::start().await;
        app_test_support::mount_workspace_routing(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/codex/config/bundle"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let profile =
            profiles.allocate_profile(Some("Synthetic quota".into()), /*priority*/ 10)?;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": "fixture-owner",
                    "chatgpt_account_id": "fixture-account"
                }
            })
            .to_string(),
        );
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "tokens": {"id_token": format!("e30.{claims}.sig"), "access_token": "synthetic-access",
                    "refresh_token": "synthetic-refresh", "account_id": "fixture-account"},
                "last_refresh": "2099-01-01T00:00:00Z"
            }))?,
        )?;
        profiles.complete_profile(&profile.id)?;
        let reset = DateTime::from_timestamp(Utc::now().timestamp() + 7200, /*nsecs*/ 0).unwrap();
        let cached: codex_login::AccountRuntimeProfileState = serde_json::from_value(json!({
            "profile_id": profile.id, "exhausted_until": reset, "backend_resets_at": reset,
            "rate_limits": {
                "primary": {"used_percent": 41, "resets_at": reset, "window_minutes": 300},
                "secondary": {"used_percent": 31, "resets_at": reset, "window_minutes": 10080},
                "observed_at": Utc::now() - chrono::Duration::minutes(28)
            }
        }))?;
        AccountRuntimeStateStore::new(home.path().to_path_buf()).save(&serde_json::from_value(
            json!({
                "active_profile_id": profile.id, "profiles": [cached]
            }),
        )?)?;
        let mut config = codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        config.chatgpt_base_url = format!("{}/backend-api", server.uri());
        Ok(Self {
            home,
            server,
            manager: AccountManager::new(config),
            id: profile.id,
            reset,
            cached,
        })
    }

    async fn refresh(
        &self,
        response: serde_json::Value,
    ) -> anyhow::Result<(AccountManagerResult, ManagedAccountView)> {
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&self.server)
            .await;
        let result = self
            .manager
            .execute(AccountManagerOperation::Refresh {
                profile_ids: Some(vec![self.id.to_string()]),
            })
            .await?;
        let account = self.manager.inventory().await?.accounts.remove(0);
        assert!(
            self.server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.method.as_str() == "GET")
        );
        Ok((result, account))
    }
}

#[tokio::test]
async fn omitted_primary_refresh_preserves_its_age_and_reports_incomplete_quota()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let (result, account) = fixture
        .refresh(json!({
            "account_id": "fixture-account", "user_id": "fixture-owner", "plan_type": "pro",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "secondary_window": {"used_percent": 31, "limit_window_seconds": 604800,
                    "reset_at": fixture.reset.timestamp(), "reset_after_seconds": 7200}},
            "spend_control": {"reached": false}
        }))
        .await?;
    assert!(
        result.message.contains("1 incomplete"),
        "{}",
        result.message
    );
    assert!(
        account
            .refresh
            .as_ref()
            .unwrap()
            .message
            .contains("Primary window not refreshed")
    );
    let saved = AccountRuntimeStateStore::new(fixture.home.path().to_path_buf())
        .load()?
        .profiles
        .remove(0);
    assert_eq!(
        (
            saved.rate_limits.primary.clone(),
            saved.rate_limits.primary_observed_at(),
            saved.exhausted_until,
            saved.backend_resets_at
        ),
        (
            fixture.cached.rate_limits.primary.clone(),
            fixture.cached.rate_limits.primary_observed_at(),
            fixture.cached.exhausted_until,
            fixture.cached.backend_resets_at
        )
    );
    assert!(account.rate_limits.secondary_observed_at > account.rate_limits.primary_observed_at);
    assert_eq!(account.availability, "coolingDown");
    Ok(())
}

#[tokio::test]
async fn denied_usage_with_low_percentages_is_reported_separately_from_incomplete_windows()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let (result, account) = fixture
        .refresh(json!({
            "account_id": "fixture-account", "user_id": "fixture-owner", "plan_type": "pro",
            "rate_limit": {"allowed": false, "limit_reached": true,
                "primary_window": {"used_percent": 41, "limit_window_seconds": 18000,
                    "reset_at": fixture.reset.timestamp(), "reset_after_seconds": 7200},
                "secondary_window": {"used_percent": 31, "limit_window_seconds": 604800,
                    "reset_at": fixture.reset.timestamp(), "reset_after_seconds": 7200}},
            "spend_control": {"reached": false}
        }))
        .await?;
    assert!(
        result.message.contains("1 backend denied"),
        "{}",
        result.message
    );
    assert!(
        account
            .refresh
            .as_ref()
            .unwrap()
            .message
            .contains("Backend denies ordinary usage")
    );
    assert_eq!(account.availability, "coolingDown");
    Ok(())
}

#[tokio::test]
async fn rejected_older_reset_does_not_report_a_fresh_primary_window() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let (result, account) = fixture.refresh(json!({
        "account_id": "fixture-account", "user_id": "fixture-owner", "plan_type": "pro",
        "rate_limit": {"allowed": true, "limit_reached": false,
            "primary_window": {"used_percent": 41, "limit_window_seconds": 18000,
                "reset_at": (fixture.reset - chrono::Duration::minutes(5)).timestamp(), "reset_after_seconds": 6900}}
    })).await?;
    assert!(
        result.message.contains("1 incomplete"),
        "{}",
        result.message
    );
    assert!(
        account
            .refresh
            .as_ref()
            .unwrap()
            .message
            .contains("Primary window not refreshed")
    );
    assert_eq!(
        account.rate_limits.primary_observed_at,
        fixture
            .cached
            .rate_limits
            .primary_observed_at()
            .map(|at| at.timestamp())
    );
    Ok(())
}

#[tokio::test]
async fn quota_request_error_keeps_cached_windows_and_surfaces_the_actual_error()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(503).set_body_string("synthetic-unavailable"))
        .mount(&fixture.server)
        .await;
    let result = fixture
        .manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
        .await?;
    let account = fixture.manager.inventory().await?.accounts.remove(0);
    let status = account.refresh.as_ref().unwrap();
    assert!(result.message.contains("1 failed"));
    assert!(result.message.contains(&status.message));
    assert!(status.message.contains("503"));
    assert!(!status.succeeded);
    let saved = AccountRuntimeStateStore::new(fixture.home.path().to_path_buf())
        .load()?
        .profiles
        .remove(0);
    assert_eq!(saved, fixture.cached);
    assert_eq!(account.availability, "coolingDown");
    Ok(())
}
