use std::fs::OpenOptions;
use std::sync::mpsc;
use std::time::Instant;

use base64::Engine as _;
use chrono::Utc;
use codex_login::AccountProfileStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;
use crate::account_pool_recovery::RecoveryWaitBudget;
use crate::account_pool_recovery::wait_for_recovery;
use crate::config::ConfigBuilder;

struct RecoveryFixture {
    _home: tempfile::TempDir,
    config: Config,
    execution: ExecutionAuth,
}

async fn exhausted_fixture(server: &MockServer, count: usize) -> anyhow::Result<RecoveryFixture> {
    let home = tempfile::tempdir()?;
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    for index in 0..count {
        let profile = profiles.allocate_profile(Some(format!("seat-{index}")), index as u32)?;
        let owner = format!("seat-{index}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({
            "https://api.openai.com/auth": {"chatgpt_user_id": owner, "chatgpt_account_id": owner},
        }).to_string());
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "tokens": {"id_token": format!("e30.{payload}.sig"), "access_token": owner,
                    "refresh_token": "synthetic-refresh", "account_id": owner},
                "last_refresh": "2099-01-01T00:00:00Z",
            }))?,
        )?;
        profiles.complete_profile(&profile.id)?;
    }
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.chatgpt_base_url = format!("{}/backend-api", server.uri());
    config.account_pool.window_warmup = Some(false);
    let execution = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(execution.ensure_runtime_from_config(&config).await?);
    let pool = execution.account_pool().expect("pool");
    while let Ok(lease) = pool.lease() {
        pool.mark_exhausted(&lease, Some(Utc::now() + chrono::Duration::days(2)))?;
    }
    AccountRuntimeStateStore::new(config.codex_home.to_path_buf()).synchronize(&pool)?;
    Ok(RecoveryFixture {
        _home: home,
        config,
        execution,
    })
}

fn usage_response(request: &wiremock::Request, recovered_seat: Option<&str>) -> ResponseTemplate {
    let owner = request
        .headers
        .get("authorization")
        .expect("bound auth")
        .to_str()
        .expect("synthetic bearer")
        .strip_prefix("Bearer ")
        .expect("bearer");
    let allowed = recovered_seat == Some(owner);
    let short_reset = (Utc::now() + chrono::Duration::hours(2)).timestamp();
    let long_reset = (Utc::now() + chrono::Duration::days(2)).timestamp();
    ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
        "plan_type": "pro", "account_id": owner, "user_id": owner,
        "rate_limit": {"allowed": allowed, "limit_reached": !allowed,
            "primary_window": {"used_percent": if allowed { 0 } else { 100 },
                "limit_window_seconds": 18000, "reset_after_seconds": 7200, "reset_at": short_reset},
            "secondary_window": {"used_percent": if allowed { 0 } else { 100 },
                "limit_window_seconds": 604800, "reset_after_seconds": 172800, "reset_at": long_reset}},
    }))
}

fn last_candidate_owner(fixture: &RecoveryFixture) -> String {
    let pool = fixture.execution.account_pool().expect("pool");
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    candidate_keys(&pool, &store, &fixture.config)
        .expect("candidate keys")
        .last()
        .expect("last candidate")
        .snapshot
        .profile
        .label
        .clone()
        .expect("synthetic owner label")
}

#[tokio::test]
async fn successive_failed_turns_recover_the_fifth_seat_without_waiting() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let fixture = exhausted_fixture(&server, /*count*/ 5).await?;
    let restored_owner = last_candidate_owner(&fixture);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            usage_response(request, Some(restored_owner.as_str()))
        })
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let cancellation = CancellationToken::new();
    assert!(!probe_for_recovery(&fixture.execution, &fixture.config, &cancellation).await);
    assert!(probe_for_recovery(&fixture.execution, &fixture.config, &cancellation).await);
    assert!(fixture.execution.active_lease().is_some());
    Ok(())
}

#[tokio::test]
async fn final_recovery_checks_the_fifth_seat_in_the_same_pass() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let fixture = exhausted_fixture(&server, /*count*/ 5).await?;
    let restored_owner = last_candidate_owner(&fixture);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            usage_response(request, Some(restored_owner.as_str()))
        })
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    assert_eq!(
        probe_for_final_recovery(
            &fixture.execution,
            &fixture.config,
            &CancellationToken::new()
        )
        .await,
        FinalRecoveryOutcome::Recovered,
    );
    assert!(fixture.execution.active_lease().is_some());
    Ok(())
}

#[tokio::test]
async fn final_recovery_reenters_an_earlier_seat_after_its_natural_deadline() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 3).await?;
    let pool = fixture.execution.account_pool().expect("pool");
    let first = pool
        .snapshots()
        .first()
        .expect("first seat")
        .profile
        .id
        .clone();
    pool.reset_rate_limits(&first)?;
    let old = pool.lease()?;
    let deadline = Utc::now() + chrono::Duration::milliseconds(200);
    pool.mark_exhausted(&old, Some(deadline))?;
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    store.synchronize(&pool)?;
    tokio::time::sleep((deadline - Utc::now()).to_std().unwrap_or_default()).await;

    assert_eq!(
        probe_for_final_recovery(
            &fixture.execution,
            &fixture.config,
            &CancellationToken::new()
        )
        .await,
        FinalRecoveryOutcome::Recovered,
    );
    let current = pool.lease()?;
    assert_eq!(current.profile().id, first);
    assert!(current.generation() > old.generation());
    assert!(matches!(
        pool.mark_exhausted(&old, Some(Utc::now() + chrono::Duration::days(2)))?,
        codex_login::AccountAvailabilityMutation::StaleIgnored { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn final_recovery_does_not_report_unknown_permissions_as_exhausted() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "plan_type": "pro", "rate_limit": {"allowed": false, "limit_reached": true},
        })))
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 5).await?;
    assert_eq!(
        probe_for_final_recovery(
            &fixture.execution,
            &fixture.config,
            &CancellationToken::new()
        )
        .await,
        FinalRecoveryOutcome::Incomplete {
            checked: 0,
            total: 5
        },
    );
    Ok(())
}

#[tokio::test]
async fn recovered_permission_cannot_immediately_reauthorize_a_refused_request()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| usage_response(request, Some("seat-0")))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 1).await?;
    let cancellation = CancellationToken::new();
    assert!(probe_for_recovery(&fixture.execution, &fixture.config, &cancellation).await);
    let pool = fixture.execution.account_pool().expect("pool");
    let lease = pool.lease()?;
    pool.mark_exhausted(&lease, Some(Utc::now() + chrono::Duration::hours(2)))?;
    AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf()).synchronize(&pool)?;
    assert_eq!(
        probe_for_final_recovery(&fixture.execution, &fixture.config, &cancellation).await,
        FinalRecoveryOutcome::Incomplete {
            checked: 0,
            total: 1
        },
    );
    assert!(fixture.execution.active_lease().is_none());
    Ok(())
}

#[tokio::test]
async fn concurrent_and_immediate_retry_probes_share_one_metadata_pass() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| {
            usage_response(request, /*recovered_seat*/ None).set_delay(Duration::from_millis(20))
        })
        .expect(/*requests*/ 4)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 4).await?;
    let cancellation = CancellationToken::new();
    assert_eq!(
        tokio::join!(
            probe_for_recovery(&fixture.execution, &fixture.config, &cancellation),
            probe_for_recovery(&fixture.execution, &fixture.config, &cancellation),
        ),
        (false, false)
    );
    assert!(!probe_for_recovery(&fixture.execution, &fixture.config, &cancellation).await);
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::CompleteUnchanged
    );
    Ok(())
}

#[tokio::test]
async fn spending_coverage_checks_a_restored_fifth_seat_before_any_paid_transition()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let fixture = exhausted_fixture(&server, /*count*/ 5).await?;
    let restored_owner = last_candidate_owner(&fixture);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            usage_response(request, Some(restored_owner.as_str()))
        })
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let cancellation = CancellationToken::new();
    assert!(!probe_for_recovery(&fixture.execution, &fixture.config, &cancellation).await);
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::Recovered
    );
    Ok(())
}

#[tokio::test]
async fn unknown_owner_metadata_cannot_authorize_spending_or_repeated_metadata_reads()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "plan_type": "pro", "rate_limit": {"allowed": false, "limit_reached": true},
        })))
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 5).await?;
    let cancellation = CancellationToken::new();
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::Incomplete
    );
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::Incomplete
    );
    Ok(())
}

#[tokio::test]
async fn a_new_refusal_invalidates_only_its_seats_spending_coverage() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| {
            usage_response(request, /*recovered_seat*/ None)
        })
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 4).await?;
    let cancellation = CancellationToken::new();
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::CompleteUnchanged
    );
    let pool = fixture.execution.account_pool().expect("pool");
    let failed = fixture
        .execution
        .quota_rescue_identity()
        .expect("refused identity");
    pool.mark_exhausted(
        failed.account_lease().expect("account lease"),
        Some(Utc::now() + chrono::Duration::days(2)),
    )?;
    assert_eq!(
        coverage_for_spending(&fixture.execution, &fixture.config, &cancellation).await,
        SpendingRecoveryCoverage::CompleteUnchanged
    );
    Ok(())
}

#[tokio::test]
async fn oversized_pools_require_manual_confirmation_before_paid_rescue() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = exhausted_fixture(&server, /*count*/ 65).await?;
    assert_eq!(
        coverage_for_spending(
            &fixture.execution,
            &fixture.config,
            &CancellationToken::new()
        )
        .await,
        SpendingRecoveryCoverage::Incomplete
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn busy_shared_import_preserves_pass_deadline_and_wait_cancellation() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let profile = profiles.allocate_profile(/*label*/ None, /*priority*/ 0)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({
        "https://api.openai.com/auth": {"chatgpt_user_id": "probe-owner", "chatgpt_account_id": "probe-account"},
    }).to_string());
    std::fs::write(
        profile.credential_home.join("auth.json"),
        serde_json::to_vec(&json!({
            "tokens": {"id_token": format!("e30.{payload}.sig"), "access_token": "synthetic-access",
                "refresh_token": "synthetic-refresh", "account_id": "probe-account"},
            "last_refresh": "2099-01-01T00:00:00Z",
        }))?,
    )?;
    profiles.complete_profile(&profile.id)?;
    let mut config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    config.account_pool.window_warmup = Some(false);
    config.chatgpt_base_url = "http://127.0.0.1:9/backend-api".into();
    let execution = ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    ));
    assert!(execution.ensure_runtime_from_config(&config).await?);
    let pool = execution.account_pool().expect("pool");
    let lease = pool.lease()?;
    pool.mark_exhausted(&lease, Some(Utc::now() + chrono::Duration::hours(2)))?;
    AccountRuntimeStateStore::new(home.path().to_path_buf()).synchronize(&pool)?;

    for operation in ["probe", "cancel", "budget"] {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.path().join(".account-pool.lock"))?;
        lock.lock()?;
        let (release, wait) = mpsc::channel();
        // Bound a regressed synchronous poll as well: Tokio timeout cannot interrupt file.lock().
        let unlock = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(2));
            drop(lock);
        });
        let cancellation = CancellationToken::new();
        let budget = RecoveryWaitBudget::new(Duration::from_millis(50));
        let began = Instant::now();
        let recovered = match operation {
            "probe" => probe_for_recovery(&execution, &config, &cancellation).await,
            "cancel" => {
                let cancel = async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    cancellation.cancel();
                };
                tokio::join!(
                    wait_for_recovery(&execution, &config, &budget, &cancellation),
                    cancel
                )
                .0
            }
            "budget" => wait_for_recovery(&execution, &config, &budget, &cancellation).await,
            _ => unreachable!(),
        };
        let elapsed = began.elapsed();
        let _ = release.send(());
        unlock.join().expect("release shared lock");
        assert!(!recovered);
        assert!(
            elapsed < Duration::from_millis(500),
            "{operation} blocked for {elapsed:?}"
        );
        if operation == "budget" {
            assert_eq!(budget.remaining(), Duration::ZERO);
        }
    }
    Ok(())
}
