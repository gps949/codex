use super::*;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountProfileStore;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct ManagerFixture {
    home: tempfile::TempDir,
    server: MockServer,
    manager: Arc<AccountManager>,
    id: codex_login::AccountProfileId,
    owner_key: String,
}

enum CreditRefresh {
    Available,
    Unavailable,
}

impl ManagerFixture {
    async fn new() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let server = MockServer::start().await;
        app_test_support::mount_workspace_routing(&server).await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(
                "^/(backend-api/wham|api/codex)/config/bundle$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let profile =
            profiles.allocate_profile(Some("Synthetic menu".into()), /*priority*/ 10)?;
        write_chatgpt_auth(
            &profile.credential_home,
            ChatGptAuthFixture::new("synthetic-access")
                .account_id("synthetic-workspace")
                .chatgpt_account_id("synthetic-workspace")
                .chatgpt_user_id("synthetic-user")
                .email("synthetic@example.test")
                .plan_type("pro"),
            AuthCredentialsStoreMode::File,
        )?;
        profiles.complete_profile(&profile.id)?;
        let mut config = codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
        config.chatgpt_base_url = format!("{}/backend-api", server.uri());
        let mut auth_config = config.auth_config();
        auth_config.codex_home = profile.credential_home;
        let auth = codex_login::AuthManager::shared_managed_profile_from_auth_config(auth_config)
            .await
            .auth_cached()
            .unwrap();
        let owner_key = crate::reset_credit_journal::owner_key(&config.chatgpt_base_url, &auth)
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            home,
            server,
            manager: AccountManager::new(config),
            id: profile.id,
            owner_key,
        })
    }

    async fn mount_redemption(&self, refresh: CreditRefresh) -> Arc<AtomicBool> {
        let redeemed = Arc::new(AtomicBool::new(false));
        let consume = Arc::clone(&redeemed);
        let inventory_redemption = Arc::clone(&redeemed);
        Mock::given(method("POST"))
            .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
            .respond_with(move |_request: &wiremock::Request| {
                consume.store(true, Ordering::Release);
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"code":"reset", "windows_reset":2}))
            })
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/rate-limit-reset-credits"))
            .respond_with(move |_request: &wiremock::Request| {
                let completed = inventory_redemption.load(Ordering::Acquire);
                if completed && matches!(refresh, CreditRefresh::Unavailable) {
                    return ResponseTemplate::new(503);
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "credits": [{
                        "id": if completed { "next-credit" } else { "original-credit" },
                        "reset_type":"codex_rate_limits", "status":"available",
                        "expires_at":"2099-01-01T00:00:00Z", "granted_at":"2026-10-03T00:00:00Z"
                    }],
                    "available_count":1, "total_earned_count":2,
                }))
            })
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "account_id":"synthetic-workspace", "user_id":"synthetic-user", "plan_type":"pro",
                "rate_limit":{"allowed":true,"limit_reached":false,
                    "primary_window":{"used_percent":0,"reset_at":chrono::Utc::now().timestamp()+7200,
                        "limit_window_seconds":18000,"reset_after_seconds":7200},
                    "secondary_window":{"used_percent":0,"reset_at":chrono::Utc::now().timestamp()+7200,
                        "limit_window_seconds":604800,"reset_after_seconds":7200}}
            })))
            .mount(&self.server)
            .await;
        redeemed
    }

    async fn pending_retry(&self) -> anyhow::Result<NativeMenuSession> {
        crate::reset_credit_journal::ManualResetCreditJournal::load(self.home.path())
            .map_err(anyhow::Error::msg)?
            .remember(&self.owner_key, "original-operation", "original-credit")
            .map_err(anyhow::Error::msg)?;
        let mut session = NativeMenuSession::new(
            FrozenAccountInventory::from_inventory(self.manager.inventory().await?),
            NativeMenuEntry::Manage,
        );
        session.bind_targets(&self.manager);
        session
            .execute_read(
                MenuOperation::Credits(0),
                &self.manager,
                NativeAccountLanguage::English,
                &crate::account_management::AccountOperationContext::Independent,
            )
            .await?;
        session.prepare(
            MenuOperation::RetryPendingCredit(0),
            NativeAccountLanguage::English,
        )?;
        Ok(session)
    }
}

#[tokio::test]
async fn successful_pending_reset_retry_clears_only_its_tuple_and_reloads_terminal_credits()
-> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    fixture.mount_redemption(CreditRefresh::Available).await;
    let mut session = fixture.pending_retry().await?;
    let other_binding = ("b".repeat(64), "original-credit".into());
    session
        .redemption_keys
        .insert(other_binding.clone(), "other-operation".into());
    session
        .handle(
            MenuAnswer::Action(MenuAction::Apply),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        )
        .await?;
    assert_eq!(session.pending_reset, None);
    assert_eq!(
        session.redemption_keys,
        std::collections::HashMap::from([(other_binding, "other-operation".into())])
    );
    assert_eq!(
        (session.page, session.return_page),
        (MenuPage::Result, MenuPage::Credits(0, 0))
    );
    assert_eq!(
        session
            .credits
            .iter()
            .map(|credit| credit.id.as_str())
            .collect::<Vec<_>>(),
        vec!["next-credit"]
    );
    let credits = fixture
        .manager
        .execute(AccountManagerOperation::Credits {
            profile_id: fixture.id.to_string(),
        })
        .await?;
    assert_eq!(credits.data["pendingResetCredit"], serde_json::Value::Null);
    let posts = fixture
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method == "POST")
        .map(|request| serde_json::from_slice::<serde_json::Value>(&request.body))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        posts,
        vec![serde_json::json!({
            "redeem_request_id":"original-operation", "credit_id":"original-credit"
        })]
    );
    session.page = session.return_page;
    insta::assert_snapshot!(render(session.question(NativeAccountLanguage::English)), @r###"
    Choose a reset credit
    Credit 1
    Available · resets Codex quota
    Expires: 2099-01-01T00:00:00Z

    Back
    "###);
    Ok(())
}

#[tokio::test]
async fn terminal_previous_reset_accepts_a_null_pending_check_without_another_consume()
-> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    fixture.mount_redemption(CreditRefresh::Available).await;
    let mut session = fixture.pending_retry().await?;
    crate::reset_credit_journal::ManualResetCreditJournal::load(fixture.home.path())
        .map_err(anyhow::Error::msg)?
        .complete(
            &fixture.owner_key,
            "original-operation",
            codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome::Reset,
        )
        .map_err(anyhow::Error::msg)?;
    session
        .handle(
            MenuAnswer::Action(MenuAction::Apply),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        )
        .await?;
    assert_eq!(session.pending_reset, None);
    assert!(session.redemption_keys.is_empty());
    assert_eq!(
        session.notice,
        "Original reset operation already completed. Refresh quota for its current status."
    );
    assert!(
        fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method == "GET")
    );
    Ok(())
}

#[tokio::test]
async fn successful_reset_keeps_its_receipt_when_the_followup_credit_read_fails()
-> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    fixture.mount_redemption(CreditRefresh::Unavailable).await;
    let mut session = fixture.pending_retry().await?;
    session
        .handle(
            MenuAnswer::Action(MenuAction::Apply),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        )
        .await?;
    assert_eq!(
        (session.page, session.return_page, session.pending_reset),
        (MenuPage::Result, MenuPage::Detail(0), None)
    );
    assert!(
        session.notice.starts_with("Reset credit redeemed"),
        "{}",
        session.notice
    );
    assert!(session.credits.is_empty());
    assert!(session.credit_inventory_error.is_some());
    Ok(())
}

#[tokio::test]
async fn successful_reset_does_not_clear_another_owners_pending_session() -> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    fixture.mount_redemption(CreditRefresh::Available).await;
    let mut session = fixture.pending_retry().await?;
    let other = NativePendingReset {
        owner_key: "b".repeat(64),
        idempotency_key: "other-operation".into(),
        credit_id: Some("original-credit".into()),
    };
    session.pending_reset = Some(other.clone());
    session.credit_owner_key = Some(other.owner_key.clone());
    let other_binding = (other.owner_key.clone(), "original-credit".into());
    session
        .redemption_keys
        .insert(other_binding.clone(), other.idempotency_key.clone());
    session
        .handle(
            MenuAnswer::Action(MenuAction::Apply),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        )
        .await?;
    assert_eq!(
        (session.pending_reset, session.credit_owner_key),
        (Some(other.clone()), Some(other.owner_key))
    );
    assert_eq!(
        session.redemption_keys,
        std::collections::HashMap::from([(other_binding, "other-operation".into())])
    );
    Ok(())
}

#[tokio::test]
async fn completed_login_check_reloads_the_enrollment_and_rebinds_its_identity()
-> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    let store = AccountProfileStore::new(fixture.home.path().to_path_buf());
    let enrolled = store.allocate_profile(Some("Completed login".into()), /*priority*/ 20)?;
    let mut session = NativeMenuSession::new(
        FrozenAccountInventory::from_inventory(fixture.manager.inventory().await?),
        NativeMenuEntry::Manage,
    );
    session.bind_targets(&fixture.manager);
    session.page = MenuPage::Login;
    session.list_origin = MenuOrigin::Overview(0);
    session.login = Some(LoginProgress {
        operation_id: "completed-fixture-login".into(),
        profile_id: Some(enrolled.id.to_string()),
        verification_url: None,
        user_code: None,
        status: "completed".into(),
        message: "Signed in".into(),
    });
    write_chatgpt_auth(
        &enrolled.credential_home,
        ChatGptAuthFixture::new("synthetic-new-access")
            .account_id("new-workspace")
            .chatgpt_account_id("new-workspace")
            .chatgpt_user_id("new-user")
            .email("new@example.test")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    store.complete_profile(&enrolled.id)?;
    session
        .handle(
            MenuAnswer::Action(MenuAction::Execute(MenuOperation::LoginCheck)),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        )
        .await?;
    let index = session
        .inventory
        .accounts
        .iter()
        .position(|account| account.id == enrolled.id.as_str())
        .unwrap();
    let account = &session.inventory.accounts[index];
    assert_eq!(
        (
            account.login_state.as_str(),
            account.state.as_str(),
            session.page,
            session.list_origin
        ),
        (
            "signedIn",
            "ready",
            MenuPage::Login,
            MenuOrigin::Overview(0)
        )
    );
    assert_eq!(
        session
            .identities
            .get(enrolled.id.as_str())
            .cloned()
            .flatten(),
        Some(fixture.manager.profile_identity(enrolled.id.as_str())?)
    );
    assert!(matches!(
        session
            .handle(
                MenuAnswer::Action(MenuAction::SelectSubscription(index)),
                &fixture.manager,
                NativeAccountLanguage::English,
                &crate::account_management::AccountOperationContext::Independent
            )
            .await?,
        MenuOutcome::Completed(_)
    ));
    assert_eq!(
        codex_login::AccountRuntimeStateStore::new(fixture.home.path().to_path_buf())
            .load()?
            .active_profile_id,
        Some(enrolled.id)
    );
    Ok(())
}

#[tokio::test]
async fn ended_login_check_removes_an_abandoned_enrollment_from_the_menu() -> anyhow::Result<()> {
    for status in ["cancelled", "failed"] {
        let fixture = ManagerFixture::new().await?;
        let store = AccountProfileStore::new(fixture.home.path().to_path_buf());
        let abandoned =
            store.allocate_profile(Some("Abandoned login".into()), /*priority*/ 20)?;
        let mut session = NativeMenuSession::new(
            FrozenAccountInventory::from_inventory(fixture.manager.inventory().await?),
            NativeMenuEntry::Manage,
        );
        session.bind_targets(&fixture.manager);
        session.page = MenuPage::Login;
        session.return_page = MenuPage::Detail(1);
        session.login = Some(LoginProgress {
            operation_id: "ended-fixture-login".into(),
            profile_id: Some(abandoned.id.to_string()),
            verification_url: None,
            user_code: None,
            status: status.into(),
            message: "Login ended".into(),
        });
        store.remove_profile_metadata(&abandoned.id)?;
        session
            .handle(
                MenuAnswer::Action(MenuAction::Execute(MenuOperation::LoginCheck)),
                &fixture.manager,
                NativeAccountLanguage::English,
                &crate::account_management::AccountOperationContext::Independent,
            )
            .await?;
        assert_eq!(
            session
                .inventory
                .accounts
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>(),
            vec![fixture.id.as_str()]
        );
        assert_eq!(
            (session.page, session.return_page),
            (MenuPage::Login, MenuPage::Overview(0))
        );
        assert!(!session.identities.contains_key(abandoned.id.as_str()));
    }
    Ok(())
}

#[tokio::test]
async fn outer_menu_timeout_keeps_a_received_reset_receipt_during_the_optional_credit_read()
-> anyhow::Result<()> {
    let fixture = ManagerFixture::new().await?;
    let redeemed = fixture.mount_redemption(CreditRefresh::Available).await;
    let mut session = fixture.pending_retry().await?;
    let entered = Arc::new(tokio::sync::Notify::new());
    let observing = Arc::clone(&entered);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            if redeemed.load(Ordering::Acquire) {
                observing.notify_one();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"available_count":0,"credits":[]}))
                    .set_delay(std::time::Duration::from_secs(3))
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "available_count":1,"credits":[{"id":"original-credit","reset_type":"codex_rate_limits",
                        "status":"available","granted_at":"2026-10-03T00:00:00Z","expires_at":null}]
                }))
            }
        }).with_priority(/*p*/ 1).mount(&fixture.server).await;
    {
        let apply = session.handle(
            MenuAnswer::Action(MenuAction::Apply),
            &fixture.manager,
            NativeAccountLanguage::English,
            &crate::account_management::AccountOperationContext::Independent,
        );
        tokio::pin!(apply);
        tokio::select! {
            result = &mut apply => panic!("expected optional read still pending: {}", result.is_ok()),
            _ = entered.notified() => {},
        }
    }
    session.error(
        anyhow::anyhow!("synthetic operation timeout"),
        NativeAccountLanguage::English,
    );
    assert!(
        session.notice.starts_with("Reset credit redeemed"),
        "{}",
        session.notice
    );
    assert_eq!(
        (session.page, session.return_page, session.pending_reset),
        (MenuPage::Result, MenuPage::Detail(0), None)
    );
    Ok(())
}
