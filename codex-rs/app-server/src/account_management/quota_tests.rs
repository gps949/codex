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
            .and(wiremock::matchers::path_regex(
                "^/(backend-api/wham|api/codex)/config/bundle$",
            ))
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

#[tokio::test]
async fn manager_replays_the_original_credit_after_a_lost_response_and_restart()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let posts = Arc::new(std::sync::atomic::AtomicUsize::default());
    let spent = Arc::clone(&posts);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            let spent = spent.load(std::sync::atomic::Ordering::SeqCst) > 0;
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "available_count": if spent { 0 } else { 1 },
                "credits": if spent { vec![] } else { vec![json!({
                    "id":"original-credit", "reset_type":"codex_rate_limits",
                    "status":"available", "granted_at":"2026-01-01T00:00:00Z",
                    "expires_at":null
                })] }
            }))
        })
        .mount(&fixture.server)
        .await;
    let observed_posts = Arc::clone(&posts);
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .and(wiremock::matchers::body_json(json!({
            "redeem_request_id":"original-operation", "credit_id":"original-credit"
        })))
        .respond_with(move |_: &wiremock::Request| {
            if observed_posts.fetch_add(/*val*/ 1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(/*s*/ 503)
            } else {
                ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                    "code":"already_redeemed", "windows_reset":0
                }))
            }
        })
        .expect(/*r*/ 2)
        .mount(&fixture.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(/*s*/ 503))
        .mount(&fixture.server)
        .await;
    let credits = fixture
        .manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    let expected_owner = credits.data["resetOwnerKey"]
        .as_str()
        .expect("credit confirmation includes its seat owner")
        .to_owned();
    let operation = || {
        serde_json::from_value::<AccountManagerOperation>(json!({
            "type":"redeem", "profileId":fixture.id, "creditId":"original-credit",
            "idempotencyKey":"original-operation", "expectedOwnerKey":expected_owner
        }))
    };
    assert!(fixture.manager.execute(operation()?).await.is_err());
    let manager = AccountManager::new((*fixture.manager.config).clone());
    let credits = manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    assert_eq!(
        credits.data["pendingResetCredit"],
        json!({
            "ownerKey":expected_owner, "idempotencyKey":"original-operation",
            "creditId":"original-credit"
        })
    );
    let replacement = serde_json::from_value::<AccountManagerOperation>(json!({
        "type":"redeem", "profileId":fixture.id, "creditId":"replacement-credit",
        "idempotencyKey":"replacement-operation", "expectedOwnerKey":expected_owner
    }))?;
    assert!(
        manager
            .execute(replacement)
            .await
            .unwrap_err()
            .to_string()
            .contains("original operation")
    );
    manager.execute(operation()?).await?;
    let credits = manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    assert_eq!(credits.data["pendingResetCredit"], serde_json::Value::Null);
    assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn manager_recovers_a_pending_reset_when_credit_inventory_fails_after_restart()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let posts = Arc::new(std::sync::atomic::AtomicUsize::default());
    let inventory_posts = Arc::clone(&posts);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            if inventory_posts.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                ResponseTemplate::new(/*s*/ 503).set_body_string("synthetic-inventory-unavailable")
            } else {
                ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                    "available_count": 1, "credits": [{
                        "id":"original-credit", "reset_type":"codex_rate_limits",
                        "status":"available", "granted_at":"2026-01-01T00:00:00Z",
                        "expires_at":null
                    }]
                }))
            }
        })
        .mount(&fixture.server)
        .await;
    let observed_posts = Arc::clone(&posts);
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .and(wiremock::matchers::body_json(json!({
            "redeem_request_id":"original-operation", "credit_id":"original-credit"
        })))
        .respond_with(move |_: &wiremock::Request| {
            if observed_posts.fetch_add(/*val*/ 1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(/*s*/ 503)
            } else {
                ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                    "code":"already_redeemed", "windows_reset":0
                }))
            }
        })
        .expect(/*r*/ 2)
        .mount(&fixture.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(/*s*/ 503))
        .mount(&fixture.server)
        .await;
    let confirmation = fixture
        .manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    let original_owner = confirmation.data["resetOwnerKey"]
        .as_str()
        .expect("confirmation includes the original seat")
        .to_owned();
    let operation = || {
        serde_json::from_value::<AccountManagerOperation>(json!({
            "type":"redeem", "profileId":fixture.id, "creditId":"original-credit",
            "idempotencyKey":"original-operation", "expectedOwnerKey":original_owner
        }))
    };
    assert!(fixture.manager.execute(operation()?).await.is_err());
    let journal_path = fixture
        .home
        .path()
        .join(".manual-rate-limit-reset-credits.json");
    let pending_before_read = std::fs::read(&journal_path)?;
    let manager = AccountManager::new((*fixture.manager.config).clone());
    let recovery = manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    let inventory_error = recovery.data["inventoryError"]
        .as_str()
        .expect("unknown inventory reports its read error")
        .to_owned();
    assert!(inventory_error.contains("503"), "{inventory_error}");
    assert_eq!(
        recovery.data,
        json!({
            "profileId":fixture.id,"availableCount":null,"credits":[],
            "resetOwnerKey":original_owner,"inventoryError":inventory_error,
            "pendingResetCredit":{
                "ownerKey":original_owner,"idempotencyKey":"original-operation",
                "creditId":"original-credit"
            }
        })
    );
    assert_eq!(std::fs::read(&journal_path)?, pending_before_read);
    assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let replacement = serde_json::from_value::<AccountManagerOperation>(json!({
        "type":"redeem", "profileId":fixture.id, "creditId":"replacement-credit",
        "idempotencyKey":"replacement-operation", "expectedOwnerKey":original_owner
    }))?;
    assert!(
        manager
            .execute(replacement)
            .await
            .unwrap_err()
            .to_string()
            .contains("original operation")
    );
    manager.execute(operation()?).await?;
    assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        crate::reset_credit_journal::ManualResetCreditJournal::load(fixture.home.path())
            .map_err(anyhow::Error::msg)?
            .pending_for_owner(&original_owner),
        None
    );
    let error = manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("503"), "{error}");
    Ok(())
}

#[tokio::test]
async fn pending_credit_recovery_rejects_an_owner_change_during_a_failed_inventory_read()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let (_, _, auth) = fixture.manager.profile_client(fixture.id.as_str()).await?;
    let owner =
        crate::reset_credit_journal::owner_key(&fixture.manager.config.chatgpt_base_url, &auth)
            .map_err(anyhow::Error::msg)?;
    let store = AccountRuntimeStateStore::new(fixture.home.path().to_path_buf());
    let lock = store
        .try_lock_reset_credit()?
        .expect("hold the shared spending lock");
    let mut journal =
        crate::reset_credit_journal::ManualResetCreditJournal::load(fixture.home.path())
            .map_err(anyhow::Error::msg)?;
    journal
        .remember(&owner, "original-operation", "original-credit")
        .map_err(anyhow::Error::msg)?;
    let pending = journal.pending_for_owner(&owner);
    drop(lock);
    let credential_home = fixture
        .manager
        .profile(fixture.id.as_str())?
        .profile
        .credential_home;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            app_test_support::write_chatgpt_auth(
                &credential_home,
                app_test_support::ChatGptAuthFixture::new("different-seat-synthetic-token")
                    .account_id("fixture-account")
                    .chatgpt_account_id("fixture-account")
                    .chatgpt_user_id("another-seat")
                    .plan_type("business"),
                codex_config::types::AuthCredentialsStoreMode::File,
            )
            .expect("replace only the synthetic seat during inventory GET");
            ResponseTemplate::new(/*s*/ 503)
        })
        .mount(&fixture.server)
        .await;
    let error = fixture
        .manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Account identity changed during the credit check"),
        "{error}"
    );
    assert_eq!(
        crate::reset_credit_journal::ManualResetCreditJournal::load(fixture.home.path())
            .map_err(anyhow::Error::msg)?
            .pending_for_owner(&owner),
        pending
    );
    assert!(
        fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "POST")
    );
    Ok(())
}

#[tokio::test]
async fn manager_rejects_a_confirmation_after_the_same_profile_changes_business_seat()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "available_count":1,"credits":[{"id":"same-credit","reset_type":"codex_rate_limits",
                "status":"available","granted_at":"2026-01-01T00:00:00Z","expires_at":null}]
        })))
        .mount(&fixture.server)
        .await;
    let confirmation = fixture
        .manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    let profile = fixture.manager.profile(fixture.id.as_str())?;
    app_test_support::write_chatgpt_auth(
        &profile.profile.credential_home,
        app_test_support::ChatGptAuthFixture::new("different-seat-synthetic-token")
            .account_id("fixture-account")
            .chatgpt_account_id("fixture-account")
            .chatgpt_user_id("another-seat")
            .plan_type("business"),
        codex_config::types::AuthCredentialsStoreMode::File,
    )?;
    let redeem = serde_json::from_value::<AccountManagerOperation>(json!({
        "type":"redeem","profileId":fixture.id,"creditId":"same-credit",
        "idempotencyKey":"old-confirmation", "expectedOwnerKey":confirmation.data["resetOwnerKey"]
    }))?;
    let error = fixture
        .manager
        .execute(redeem)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Account changed since reset confirmation"),
        "{error}"
    );
    assert!(
        fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() != "POST")
    );
    Ok(())
}
