use chrono::Duration;
use chrono::Utc;
use codex_config::AutoResetCredits;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountProfile;
use codex_login::AccountProfileId;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::TokenData;
use codex_login::save_auth;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::consume_reset_credit_for_profile;
use super::reactivate_redeemed_profile;
use super::should_redeem;
use super::try_reset_credit_rescue;
use crate::config::ConfigBuilder;
use crate::execution_auth::ExecutionAuth;

struct CreditRescueFixture {
    _home: tempfile::TempDir,
    config: crate::config::Config,
    execution: ExecutionAuth,
    failed: crate::execution_auth::ExecutionAuthLease,
    standby: AccountProfileId,
    reset: chrono::DateTime<Utc>,
}

async fn exhausted_credit_fixture(base_url: String) -> anyhow::Result<CreditRescueFixture> {
    use base64::Engine as _;
    let home = tempfile::tempdir()?;
    let profiles = codex_login::AccountProfileStore::new(home.path().to_path_buf());
    for (name, priority) in [("primary", 0), ("standby", 10)] {
        let profile = profiles.allocate_profile(Some(name.to_string()), priority)?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &json!({"https://api.openai.com/auth": {
                "chatgpt_user_id": name, "chatgpt_account_id": name, "chatgpt_plan_type": "pro",
            }}),
        )?);
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "tokens": {"id_token": format!("e30.{payload}.sig"), "access_token": format!("{name}-access"),
                    "refresh_token": format!("{name}-refresh"), "account_id": name},
                "last_refresh": "2099-01-01T00:00:00Z",
            }))?,
        )?;
        profiles.complete_profile(&profile.id)?;
    }
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = base_url;
    config.account_pool.window_warmup = Some(false);
    config.account_pool.auto_reset_credits = Some(AutoResetCredits::WhenPoolExhausted);
    let execution = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(execution.ensure_runtime_from_config(&config).await?);
    let pool = execution.account_pool().expect("pool");
    let failed = execution.active_lease().expect("failed lease");
    let reset = Utc::now() + Duration::hours(2);
    pool.mark_exhausted(failed.account_lease().expect("pooled lease"), Some(reset))?;
    let standby_lease = pool.lease()?;
    let standby = standby_lease.profile().id.clone();
    pool.mark_exhausted(&standby_lease, Some(reset))?;
    codex_login::AccountRuntimeStateStore::new(config.codex_home.to_path_buf())
        .synchronize(&pool)?;
    Ok(CreditRescueFixture {
        _home: home,
        config,
        execution,
        failed,
        standby,
        reset,
    })
}

#[tokio::test]
async fn credit_lock_imports_another_profiles_shared_recovery_before_spending() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = exhausted_credit_fixture(format!("{}/backend-api", server.uri())).await?;
    let store = codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let lock = store
        .try_lock_reset_credit()?
        .expect("hold shared spending lock");
    let rescue = try_reset_credit_rescue(&fixture.execution, &fixture.failed, &fixture.config);
    tokio::pin!(rescue);
    assert!(futures::poll!(rescue.as_mut()).is_pending());
    store.record_quota_reset(&fixture.standby, Utc::now())?;
    drop(lock);
    let rescue = rescue
        .await
        .expect("recover using the other process's free account");
    assert_eq!(
        (rescue.profile_id, rescue.redeemed_profile_id),
        (fixture.standby, None)
    );
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);
    Ok(())
}

#[tokio::test]
async fn credit_response_cannot_erase_a_newer_refusal_with_the_same_reset_time()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let fixture = exhausted_credit_fixture(format!("{}/backend-api", server.uri())).await?;
    let pool = fixture.execution.account_pool().expect("pool");
    let failed = fixture
        .failed
        .account_lease()
        .expect("pooled lease")
        .clone();
    let reset = fixture.reset;
    let responding_pool = Arc::clone(&pool);
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(move |_request: &wiremock::Request| {
            let latest = responding_pool
                .force_activate(&failed.profile().id)
                .expect("new explicit request");
            responding_pool
                .mark_exhausted(&latest, Some(reset))
                .expect("new refusal");
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2}))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let rescued =
        try_reset_credit_rescue(&fixture.execution, &fixture.failed, &fixture.config).await;
    assert!(
        rescued.is_none(),
        "rescue={:?}, snapshots={:?}",
        rescued
            .as_ref()
            .map(|rescue| (&rescue.profile_id, &rescue.redeemed_profile_id)),
        pool.snapshots()
    );
    assert!(pool.snapshots().iter().all(|snapshot| matches!(
        snapshot.availability,
        codex_login::AccountAvailability::Exhausted { .. }
    )));
    let attempted = std::fs::read_dir(&fixture.config.codex_home)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".rate-limit-reset-credit-")
        })
        .count();
    assert_eq!(
        attempted, 1,
        "retain the ambiguous attempt instead of issuing another credit request"
    );
    Ok(())
}

struct CreditTestAuth(Mutex<CodexAuth>);

impl codex_login::ExternalAuth for CreditTestAuth {
    fn resolve(&self) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.lock().expect("test auth").clone()) })
    }

    fn refresh(
        &self,
        _context: codex_login::ExternalAuthRefreshContext,
    ) -> codex_login::ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

fn credit_test_token(label: &str, workspace: &str) -> String {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({
            "jti": label, "https://api.openai.com/auth": {
                "chatgpt_user_id": "credit-test-user", "chatgpt_account_id": workspace,
                "chatgpt_plan_type": "pro",
            },
        })
        .to_string(),
    );
    format!("e30.{payload}.sig")
}

async fn credit_test_auth(
    credential_home: &std::path::Path,
    label: &str,
    workspace: &str,
) -> anyhow::Result<CodexAuth> {
    let token = credit_test_token(label, workspace);
    save_auth(
        credential_home,
        &AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: parse_chatgpt_jwt_claims(&token).map_err(std::io::Error::other)?,
                access_token: token,
                refresh_token: format!("{workspace}-refresh"),
                account_id: Some(workspace.to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        },
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    CodexAuth::from_auth_storage(
        credential_home,
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &codex_login::test_support::transport_default_auth_route_config(),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("synthetic profile auth was not saved"))
}

struct CreditProfileFixture {
    id: AccountProfileId,
    manager: Arc<AuthManager>,
    source: Arc<CreditTestAuth>,
}

async fn register_credit_profile(
    config: &crate::config::Config,
    pool: &AccountPool,
    token: &str,
    workspace: &str,
    priority: u32,
) -> anyhow::Result<CreditProfileFixture> {
    let profiles = codex_login::AccountProfileStore::new(config.codex_home.to_path_buf());
    let profile = profiles.allocate_profile(/*label*/ None, priority)?;
    let auth = credit_test_auth(&profile.credential_home, token, workspace).await?;
    let source = Arc::new(CreditTestAuth(Mutex::new(auth.clone())));
    let manager =
        AuthManager::from_auth_for_testing_with_home(auth, profile.credential_home.clone());
    manager.set_external_auth(source.clone()).await?;
    profiles.complete_profile(&profile.id)?;
    let id = profile.id.clone();
    pool.register(profile, manager.clone())?;
    let lease = pool.activate(&id)?;
    pool.mark_exhausted(&lease, Some(Utc::now() + Duration::hours(2)))?;
    Ok(CreditProfileFixture {
        id,
        manager,
        source,
    })
}

struct CreditMaintenanceRoute(String);

impl codex_login::WorkspaceRoutingResolver for CreditMaintenanceRoute {
    fn maintenance_clients(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::io::Result<Option<codex_login::WorkspaceMaintenanceClients>>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Ok(Some(codex_login::WorkspaceMaintenanceClients {
                chatgpt_base_url: self.0.clone(),
                http_client_factory:
                    codex_login::test_support::transport_default_auth_route_config()
                        .http_client_factory()
                        .clone(),
            }))
        })
    }

    fn resolve(
        &self,
        _request: codex_login::WorkspaceRoutingRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<Option<codex_login::WorkspaceRouting>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn redemption_loads_the_target_profiles_maintenance_transport() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let inherited = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*requests*/ 1)
        .mount(&target)
        .await;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", inherited.uri());
    config.application_network_policy.invalidate();
    let pool = AccountPool::new();
    let profile = register_credit_profile(
        &config,
        &pool,
        "e30.e30.c2VhdA",
        "workspace",
        /*priority*/ 0,
    )
    .await?;
    let owner: Arc<dyn codex_login::WorkspaceRoutingResolver> = Arc::new(CreditMaintenanceRoute(
        format!("{}/backend-api", target.uri()),
    ));
    profile
        .manager
        .set_workspace_routing_resolver(Arc::downgrade(&owner));
    assert!(matches!(
        consume_reset_credit_for_profile(&pool, &profile.id, &config, "profile-route").await,
        super::ResetCreditOutcome::Reset(_),
    ));
    assert_eq!(
        inherited.received_requests().await.expect("requests").len(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn redemption_does_not_spend_without_the_requirements_owner() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let server = MockServer::start().await;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    let pool = AccountPool::new();
    let profile = register_credit_profile(
        &config,
        &pool,
        "e30.e30.c2VhdA",
        "workspace",
        /*priority*/ 0,
    )
    .await?;
    let owner: Arc<dyn codex_login::WorkspaceRoutingResolver> =
        Arc::new(CreditMaintenanceRoute(config.chatgpt_base_url.clone()));
    profile
        .manager
        .set_workspace_routing_resolver(Arc::downgrade(&owner));
    drop(owner);
    assert!(matches!(
        consume_reset_credit_for_profile(&pool, &profile.id, &config, "missing-owner").await,
        super::ResetCreditOutcome::Unknown,
    ));
    assert_eq!(server.received_requests().await.expect("requests").len(), 0);
    Ok(())
}

#[tokio::test]
async fn ambiguous_redemption_keeps_unknown_outcome_for_untrusted_confirmation()
-> anyhow::Result<()> {
    for confirmation in [
        json!({"account_id": "another-workspace", "rate_limit": {"allowed": true}}),
        json!({"user_id": "another-user", "rate_limit": {"allowed": true}}),
        json!({"rate_limit": {}}),
    ] {
        let home = tempfile::tempdir()?;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
            .respond_with(
                ResponseTemplate::new(/*status*/ 200)
                    .set_body_json(json!({"code": "already_redeemed"})),
            )
            .expect(/*requests*/ 1)
            .mount(&server)
            .await;
        let mut confirmation = confirmation;
        if confirmation["account_id"].is_null() {
            confirmation["account_id"] = json!("workspace");
        }
        if confirmation["user_id"].is_null() {
            confirmation["user_id"] = json!("credit-test-user");
        }
        confirmation["rate_limit"]["limit_reached"] = json!(false);
        confirmation["rate_limit"]["primary_window"] = json!({
            "used_percent": 0, "limit_window_seconds": 18000,
            "reset_after_seconds": 18000, "reset_at": 2000000000,
        });
        confirmation["rate_limit"]["secondary_window"] = json!({
            "used_percent": 0, "limit_window_seconds": 604800,
            "reset_after_seconds": 604800, "reset_at": 2000000000,
        });
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(confirmation))
            .expect(/*requests*/ 1)
            .mount(&server)
            .await;
        let mut config = ConfigBuilder::without_managed_config_for_tests()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        config.chatgpt_base_url = format!("{}/backend-api", server.uri());
        let pool = AccountPool::new();
        let profile = register_credit_profile(
            &config,
            &pool,
            "e30.e30.c2VhdA",
            "workspace",
            /*priority*/ 0,
        )
        .await?;
        assert!(matches!(
            consume_reset_credit_for_profile(&pool, &profile.id, &config, "same-ambiguous-id")
                .await,
            super::ResetCreditOutcome::Unknown,
        ));
    }
    Ok(())
}

#[tokio::test]
async fn redemption_response_for_a_replaced_owner_cannot_recover_the_profile() -> anyhow::Result<()>
{
    let home = tempfile::tempdir()?;
    let server = MockServer::start().await;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    let pool = AccountPool::new();
    let profile =
        register_credit_profile(&config, &pool, "old", "old-workspace", /*priority*/ 0).await?;
    let source = Arc::clone(&profile.source);
    let replacement_home = tempfile::tempdir()?;
    let next_auth = credit_test_auth(replacement_home.path(), "new", "new-workspace").await?;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .and(header("chatgpt-account-id", "old-workspace"))
        .respond_with(move |_request: &wiremock::Request| {
            *source.0.lock().expect("test auth") = next_auth.clone();
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2}))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let before = pool.snapshots();
    assert!(matches!(
        consume_reset_credit_for_profile(&pool, &profile.id, &config, "old-owner-reset").await,
        super::ResetCreditOutcome::Unknown,
    ));
    assert_eq!(pool.snapshots(), before);
    Ok(())
}

#[tokio::test]
async fn ambiguous_redemption_verifies_the_bound_profile_before_recovery() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "already_redeemed"})),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(header("chatgpt-account-id", "seat-workspace"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "account_id": "seat-workspace", "user_id": "credit-test-user",
            "plan_type": "pro", "rate_limit": {
                "allowed": true, "limit_reached": false,
                "primary_window": {"used_percent": 1, "limit_window_seconds": 18000,
                    "reset_after_seconds": 18000, "reset_at": 2000000000},
                "secondary_window": {"used_percent": 2, "limit_window_seconds": 604800,
                    "reset_after_seconds": 604800, "reset_at": 2000000000}
            }
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    let pool = AccountPool::new();
    let profile = register_credit_profile(
        &config,
        &pool,
        "e30.e30.c2VhdA",
        "seat-workspace",
        /*priority*/ 0,
    )
    .await?;
    let super::ResetCreditOutcome::AlreadyUsable(probe, observed) =
        consume_reset_credit_for_profile(&pool, &profile.id, &config, "same-ambiguous-id").await
    else {
        anyhow::bail!("owner-matched backend confirmation must verify recovery");
    };
    assert!(
        codex_login::AccountRuntimeStateStore::new(config.codex_home.to_path_buf())
            .reconcile_quota_probe(
                &pool,
                probe,
                codex_login::AccountQuotaEvidence {
                    rate_limits: &observed.rate_limits,
                    ordinary_usage_allowed: observed.ordinary_usage_allowed,
                    account_id: observed.account_id.as_deref(),
                    user_id: observed.user_id.as_deref(),
                }
            )?
    );
    assert_eq!(
        pool.snapshots()[0].availability,
        AccountAvailability::Available
    );
    Ok(())
}

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

    let home = tempfile::tempdir()?;
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
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());

    let pool = AccountPool::new();
    register_credit_profile(
        &config,
        &pool,
        PRIMARY_TOKEN,
        "account-primary",
        /*priority*/ 0,
    )
    .await?;
    let failed = register_credit_profile(
        &config,
        &pool,
        FAILED_TOKEN,
        "account-failed",
        /*priority*/ 10,
    )
    .await?;

    assert!(matches!(
        consume_reset_credit_for_profile(&pool, &failed.id, &config, "stable-request-id").await,
        super::ResetCreditOutcome::Reset(_),
    ));
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
            Some(format!(
                "Bearer {}",
                credit_test_token(FAILED_TOKEN, "account-failed")
            )),
            Some("account-failed".to_string()),
        )]
    );
    Ok(())
}

#[tokio::test]
async fn redemption_preserves_concurrent_selection_and_reports_the_consumed_seat()
-> anyhow::Result<()> {
    let pool = AccountPool::new();
    let redeemed = AccountProfileId::new("redeemed-seat")?;
    let selected = AccountProfileId::new("selected-seat")?;
    for id in [&redeemed, &selected] {
        pool.register(
            AccountProfile::new(
                id.clone(),
                std::path::PathBuf::from(id.as_str()),
                0,
                /*label*/ None,
            ),
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
        )?;
    }
    pool.activate(&selected)?;
    let rescue = reactivate_redeemed_profile(&pool, redeemed.clone()).expect("redemption recovery");
    assert_eq!(
        (
            rescue.profile_id,
            rescue.redeemed_profile_id,
            pool.lease()?.profile().id.clone()
        ),
        (selected.clone(), Some(redeemed), selected),
    );
    Ok(())
}
