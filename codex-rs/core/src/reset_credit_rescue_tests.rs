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
use tokio_util::sync::CancellationToken;
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

async fn exhausted_credit_fixture(
    base_url: String,
    count: usize,
) -> anyhow::Result<CreditRescueFixture> {
    use base64::Engine as _;
    let home = tempfile::tempdir()?;
    let profiles = codex_login::AccountProfileStore::new(home.path().to_path_buf());
    for index in 0..count {
        let name = match index {
            0 => "primary".to_string(),
            1 => "standby".to_string(),
            _ => format!("extra-{index}"),
        };
        let priority = index as u32 * 10;
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
    while let Ok(lease) = pool.lease() {
        pool.mark_exhausted(&lease, Some(reset))?;
    }
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

async fn mount_exhausted_usage(server: &MockServer, recovered: Option<&'static str>) {
    Mock::given(method("GET")).and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            let bearer = request.headers.get("authorization").expect("bound token")
                .to_str().expect("synthetic bearer");
            let owner = bearer.strip_prefix("Bearer ").expect("bearer")
                .strip_suffix("-access").expect("synthetic token");
            let allowed = recovered == Some(owner);
            let reset = (Utc::now() + Duration::hours(2)).timestamp();
            ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
                "plan_type": "pro", "account_id": owner, "user_id": owner,
                "rate_limit": {"allowed": allowed, "limit_reached": !allowed,
                    "primary_window": {"used_percent": if allowed { 0 } else { 100 },
                        "limit_window_seconds": 18000, "reset_after_seconds": 7200, "reset_at": reset},
                    "secondary_window": {"used_percent": if allowed { 0 } else { 100 },
                        "limit_window_seconds": 604800, "reset_after_seconds": 7200, "reset_at": reset}},
            }))
        }).mount(server).await;
}

#[tokio::test]
async fn credit_lock_imports_another_profiles_shared_recovery_before_spending() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, /*recovered*/ None).await;
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let store = codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let lock = store
        .try_lock_reset_credit()?
        .expect("hold shared spending lock");
    let cancellation = CancellationToken::new();
    let rescue = try_reset_credit_rescue(
        &fixture.execution,
        &fixture.failed,
        &fixture.config,
        &cancellation,
    );
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
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn credit_response_cannot_erase_a_newer_refusal_with_the_same_reset_time()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, /*recovered*/ None).await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
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
    let cancellation = CancellationToken::new();
    let rescued = try_reset_credit_rescue(
        &fixture.execution,
        &fixture.failed,
        &fixture.config,
        &cancellation,
    )
    .await;
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

#[tokio::test]
async fn restored_fifth_subscription_prevents_automatic_credit_spending() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, Some("extra-4")).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 5).await?;
    let cancellation = CancellationToken::new();
    let rescue = try_reset_credit_rescue(
        &fixture.execution,
        &fixture.failed,
        &fixture.config,
        &cancellation,
    )
    .await
    .expect("free recovered subscription");
    assert_eq!(rescue.redeemed_profile_id, None);
    assert!(fixture.execution.active_lease().is_some());
    Ok(())
}

#[tokio::test]
async fn cancellation_while_waiting_for_the_spending_lock_prevents_a_consume_post()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, /*recovered*/ None).await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2})),
        )
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let cancellation = CancellationToken::new();
    assert_eq!(
        crate::account_pool_recovery::coverage_for_spending(
            &fixture.execution,
            &fixture.config,
            &cancellation
        )
        .await,
        crate::account_pool_recovery::SpendingRecoveryCoverage::CompleteUnchanged
    );
    let store = codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    let lock = store.try_lock_reset_credit()?.expect("hold spending lock");
    let started = std::time::Instant::now();
    let cancel_and_release = async {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        cancellation.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        drop(lock);
    };
    let (rescue, ()) = tokio::join!(
        try_reset_credit_rescue(
            &fixture.execution,
            &fixture.failed,
            &fixture.config,
            &cancellation
        ),
        cancel_and_release
    );
    assert!(rescue.is_none());
    assert!(started.elapsed() < std::time::Duration::from_millis(100));
    Ok(())
}

#[tokio::test]
async fn corrupt_pending_reset_operation_blocks_automatic_spending() -> anyhow::Result<()> {
    use sha1::Digest;
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, /*recovered*/ None).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*s*/ 500))
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let id = fixture.failed.profile_id().expect("failed profile");
    let key = format!("{:x}", sha1::Sha1::digest(id.as_str().as_bytes()));
    let path = fixture
        .config
        .codex_home
        .join(format!(".rate-limit-reset-credit-{key}.json"));
    std::fs::write(&path, b"{interrupted")?;
    assert!(
        try_reset_credit_rescue(
            &fixture.execution,
            &fixture.failed,
            &fixture.config,
            &CancellationToken::new()
        )
        .await
        .is_none()
    );
    assert_eq!(std::fs::read(path)?, b"{interrupted".to_vec());
    Ok(())
}

#[tokio::test]
async fn unconfirmed_manual_reset_blocks_a_new_automatic_consume_post() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*s*/ 500))
        .expect(/*r*/ 0)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let bytes = serde_json::to_vec(&json!({"version":2,"operations":[{
        "ownerDigest":"a".repeat(64),"idempotencyKey":"manual-original","creditId":"manual-credit","phase":{"state":"pending"}
    }]}))?;
    let path = fixture
        .config
        .codex_home
        .join(".manual-rate-limit-reset-credits.json");
    std::fs::write(&path, &bytes)?;
    assert!(
        try_reset_credit_rescue(
            &fixture.execution,
            &fixture.failed,
            &fixture.config,
            &CancellationToken::new()
        )
        .await
        .is_none()
    );
    assert_eq!(std::fs::read(path)?, bytes);
    Ok(())
}

#[tokio::test]
async fn cancellation_after_a_consume_post_retains_the_ambiguous_request_id() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    mount_exhausted_usage(&server, /*recovered*/ None).await;
    let posted = Arc::new(tokio::sync::Notify::new());
    let seen = posted.clone();
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(move |_request: &wiremock::Request| {
            seen.notify_one();
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2}))
                .set_delay(std::time::Duration::from_millis(500))
        })
        .expect(/*r*/ 2)
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let cancellation = CancellationToken::new();
    let cancel = async {
        tokio::time::timeout(std::time::Duration::from_secs(2), posted.notified())
            .await
            .expect("synthetic POST started");
        cancellation.cancel();
    };
    let (rescue, ()) = tokio::join!(
        try_reset_credit_rescue(
            &fixture.execution,
            &fixture.failed,
            &fixture.config,
            &cancellation
        ),
        cancel
    );
    assert!(rescue.is_none());
    let records = std::fs::read_dir(&fixture.config.codex_home)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".rate-limit-reset-credit-")
        })
        .count();
    assert_eq!(records, 1);
    let path = std::fs::read_dir(&fixture.config.codex_home)?
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".rate-limit-reset-credit-")
        })
        .expect("pending operation")
        .path();
    let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    record["attemptedAt"] = json!(Utc::now().timestamp() - 3600);
    record
        .as_object_mut()
        .expect("legacy journal")
        .remove("attemptedAtPrecise");
    std::fs::write(&path, serde_json::to_vec(&record)?)?;
    let restarted = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(
        restarted
            .ensure_runtime_from_config(&fixture.config)
            .await?
    );
    assert!(
        try_reset_credit_rescue(
            &restarted,
            &fixture.failed,
            &fixture.config,
            &CancellationToken::new()
        )
        .await
        .is_some()
    );
    let requests = server.received_requests().await.expect("requests");
    let ids: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "POST")
        .map(|request| {
            request
                .body_json::<serde_json::Value>()
                .expect("operation request")["redeem_request_id"]
                .clone()
        })
        .collect();
    assert_eq!(
        ids,
        vec![record["requestId"].clone(), record["requestId"].clone()]
    );
    Ok(())
}

enum RecoveredPendingCandidate {
    Original,
    Other,
}

async fn confirmed_free_recovery_reconciles_pending_operation(
    candidate: RecoveredPendingCandidate,
) -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let recovered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let usage_recovered = Arc::clone(&recovered);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            let bearer = request.headers.get("authorization").expect("bound token")
                .to_str().expect("synthetic bearer");
            let owner = bearer.strip_prefix("Bearer ").expect("bearer")
                .strip_suffix("-access").expect("synthetic token");
            let allowed = owner == "primary" && usage_recovered.load(std::sync::atomic::Ordering::SeqCst);
            let reset = (Utc::now() + Duration::hours(2)).timestamp();
            ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
                "plan_type": "pro", "account_id": owner, "user_id": owner,
                "rate_limit": {"allowed": allowed, "limit_reached": !allowed,
                    "primary_window": {"used_percent": if allowed { 0 } else { 100 },
                        "limit_window_seconds": 18000, "reset_after_seconds": 7200, "reset_at": reset},
                    "secondary_window": {"used_percent": if allowed { 0 } else { 100 },
                        "limit_window_seconds": 604800, "reset_after_seconds": 7200, "reset_at": reset}},
            }))
        }).mount(&server).await;
    let posted = Arc::new(tokio::sync::Notify::new());
    let seen = Arc::clone(&posted);
    let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/backend-api/wham/rate-limit-reset-credits/consume"))
        .respond_with(move |_request: &wiremock::Request| {
            let response = ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({"code": "reset", "windows_reset": 2}));
            if posts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                seen.notify_one();
                response.set_delay(std::time::Duration::from_millis(500))
            } else {
                response
            }
        })
        .mount(&server)
        .await;
    let fixture =
        exhausted_credit_fixture(format!("{}/backend-api", server.uri()), /*count*/ 2).await?;
    let pool = fixture.execution.account_pool().expect("pool");
    let cancellation = CancellationToken::new();
    let cancel = async {
        tokio::time::timeout(std::time::Duration::from_secs(2), posted.notified())
            .await
            .expect("first POST started");
        cancellation.cancel();
    };
    let (rescue, ()) = tokio::join!(
        try_reset_credit_rescue(
            &fixture.execution,
            &fixture.failed,
            &fixture.config,
            &cancellation
        ),
        cancel
    );
    assert!(rescue.is_none());
    recovered.store(true, std::sync::atomic::Ordering::SeqCst);
    // Model a separately confirmed recovery without the original process's recent-GET cache.
    let recovery_execution = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(
        recovery_execution
            .ensure_runtime_from_config(&fixture.config)
            .await?
    );
    assert_eq!(
        crate::account_pool_recovery::coverage_for_spending(
            &recovery_execution,
            &fixture.config,
            &CancellationToken::new()
        )
        .await,
        crate::account_pool_recovery::SpendingRecoveryCoverage::Recovered
    );
    codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf())
        .synchronize(&pool)?;
    recovered.store(false, std::sync::atomic::Ordering::SeqCst);
    let recovered_lease = fixture
        .execution
        .active_lease()
        .expect("confirmed free recovery");
    pool.mark_exhausted(
        recovered_lease.account_lease().expect("pool lease"),
        Some(fixture.reset),
    )?;
    let failed = match candidate {
        RecoveredPendingCandidate::Original => recovered_lease,
        RecoveredPendingCandidate::Other => {
            pool.force_activate(&fixture.standby)?;
            let failed = fixture.execution.active_lease().expect("other candidate");
            pool.mark_exhausted(
                failed.account_lease().expect("pool lease"),
                Some(fixture.reset),
            )?;
            failed
        }
    };
    let store = codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    store.synchronize(&pool)?;
    let rescue = try_reset_credit_rescue(
        &fixture.execution,
        &failed,
        &fixture.config,
        &CancellationToken::new(),
    )
    .await
    .expect("confirmed recovery permits a fresh credit operation");
    assert_eq!(rescue.redeemed_profile_id, failed.profile_id().cloned());
    let requests = server.received_requests().await.expect("requests");
    let ids: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "POST")
        .map(|request| {
            request
                .body_json::<serde_json::Value>()
                .expect("operation request")["redeem_request_id"]
                .clone()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "the confirmed old operation cannot consume again"
    );
    let original = std::fs::read_dir(&fixture.config.codex_home)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".rate-limit-reset-credit-")
        })
        .map(|entry| -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::from_slice(&std::fs::read(entry.path())?)?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .find(|record| {
            record["scope"]["profileId"]
                == fixture
                    .failed
                    .profile_id()
                    .expect("original profile")
                    .as_str()
        })
        .expect("original journal retained");
    let confirmed = if original["requestId"] == ids[0] {
        &original
    } else {
        &original["previousConfirmed"]
    };
    assert_eq!(
        (confirmed["requestId"].clone(), confirmed["phase"].clone()),
        (
            ids[0].clone(),
            json!({"state": "confirmed", "outcome": "quotaRecovered"})
        )
    );
    assert!(confirmed["reconciledRecovery"]["observedAt"].is_string());
    Ok(())
}

#[tokio::test]
async fn confirmed_free_recovery_permits_a_new_reset_operation_after_reexhaustion()
-> anyhow::Result<()> {
    confirmed_free_recovery_reconciles_pending_operation(RecoveredPendingCandidate::Original).await
}

#[tokio::test]
async fn confirmed_free_recovery_of_pending_profile_permits_another_candidate() -> anyhow::Result<()>
{
    confirmed_free_recovery_reconciles_pending_operation(RecoveredPendingCandidate::Other).await
}

#[tokio::test]
async fn pending_reset_operation_requires_owner_bound_confirmed_recovery() -> anyhow::Result<()> {
    use super::operation::ResetCreditOperation;
    use super::operation::ResetCreditScope;
    enum UntrustedRecovery {
        SameEpoch,
        OldObservation,
        MissingObservation,
        DifferentOwner,
        ForceProbe,
        CorruptJournal,
    }
    for evidence in [
        UntrustedRecovery::SameEpoch,
        UntrustedRecovery::OldObservation,
        UntrustedRecovery::MissingObservation,
        UntrustedRecovery::DifferentOwner,
        UntrustedRecovery::ForceProbe,
        UntrustedRecovery::CorruptJournal,
    ] {
        let fixture = exhausted_credit_fixture(
            "https://synthetic.invalid/backend-api".into(),
            /*count*/ 2,
        )
        .await?;
        let pool = fixture.execution.account_pool().expect("pool");
        let profile_id = fixture.failed.profile_id().expect("profile");
        let manager = pool
            .auth_managers()
            .into_iter()
            .find(|(id, _)| id == profile_id)
            .expect("profile auth")
            .1;
        let auth = manager.auth_cached().expect("synthetic auth");
        let owner = serde_json::to_vec(&(auth.get_account_id(), auth.get_chatgpt_user_id()))?;
        use sha1::Digest as _;
        let epoch = Utc::now() - Duration::hours(2);
        let scope = ResetCreditScope {
            profile_id: profile_id.to_string(),
            owner_key: format!("{:x}", sha1::Sha1::digest(owner)),
            credit_id: None,
            reset_key: Some(123),
            quota_epoch: Some(epoch.timestamp_millis()),
        };
        let store =
            codex_login::AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
        let _lock = store.try_lock_reset_credit()?.expect("spending lock");
        ResetCreditOperation::prepare(
            &fixture.config.codex_home,
            scope.clone(),
            "original-request",
        )?;
        let path = std::fs::read_dir(&fixture.config.codex_home)?
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".rate-limit-reset-credit-")
            })
            .expect("journal")
            .path();
        let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        // Exercise legacy, seconds-only request ordering without treating elapsed time as proof.
        journal["attemptedAt"] = json!((Utc::now() - Duration::hours(1)).timestamp());
        journal
            .as_object_mut()
            .expect("journal object")
            .remove("attemptedAtPrecise");
        if matches!(evidence, UntrustedRecovery::DifferentOwner) {
            journal["scope"]["ownerKey"] = json!("another-owner");
        }
        let bytes = if matches!(evidence, UntrustedRecovery::CorruptJournal) {
            b"{interrupted".to_vec()
        } else {
            serde_json::to_vec(&journal)?
        };
        std::fs::write(&path, &bytes)?;
        let mut state = store.load()?;
        let profile = state
            .profiles
            .iter_mut()
            .find(|entry| &entry.profile_id == profile_id)
            .expect("persisted profile");
        profile.quota_reset_at = Some(Utc::now());
        profile.quota_reset_observed_at = Some(Utc::now());
        match evidence {
            UntrustedRecovery::SameEpoch => profile.quota_reset_at = Some(epoch),
            UntrustedRecovery::OldObservation => profile.quota_reset_observed_at = Some(epoch),
            UntrustedRecovery::MissingObservation => profile.quota_reset_observed_at = None,
            UntrustedRecovery::ForceProbe => {
                profile.quota_reset_at = Some(epoch);
                profile.quota_reset_observed_at = None;
            }
            UntrustedRecovery::DifferentOwner | UntrustedRecovery::CorruptJournal => {}
        }
        store.save(&state)?;
        if matches!(evidence, UntrustedRecovery::ForceProbe) {
            store.select(
                profile_id.clone(),
                codex_login::AccountSelectionMode::ForceProbe,
            )?;
        }
        super::load_synced_credit_state(
            &store,
            &pool,
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
            &CancellationToken::new(),
        )
        .await
        .expect("synchronized state");
        assert_eq!(std::fs::read(&path)?, bytes);
        let mut other = scope;
        other.profile_id = fixture.standby.to_string();
        assert!(
            ResetCreditOperation::prepare(&fixture.config.codex_home, other, "replacement-request")
                .is_err()
        );
    }
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
        consume_reset_credit_for_profile(
            &pool,
            &profile.id,
            &config,
            "profile-route",
            &CancellationToken::new()
        )
        .await,
        super::ResetCreditOutcome::Reset(..),
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
        consume_reset_credit_for_profile(
            &pool,
            &profile.id,
            &config,
            "missing-owner",
            &CancellationToken::new()
        )
        .await,
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
            consume_reset_credit_for_profile(
                &pool,
                &profile.id,
                &config,
                "same-ambiguous-id",
                &CancellationToken::new()
            )
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
        consume_reset_credit_for_profile(
            &pool,
            &profile.id,
            &config,
            "old-owner-reset",
            &CancellationToken::new()
        )
        .await,
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
    let super::ResetCreditOutcome::AlreadyUsable(probe, observed, _) =
        consume_reset_credit_for_profile(
            &pool,
            &profile.id,
            &config,
            "same-ambiguous-id",
            &CancellationToken::new(),
        )
        .await
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
        consume_reset_credit_for_profile(
            &pool,
            &failed.id,
            &config,
            "stable-request-id",
            &CancellationToken::new()
        )
        .await,
        super::ResetCreditOutcome::Reset(..),
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
