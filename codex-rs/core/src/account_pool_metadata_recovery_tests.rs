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

use super::*;
use crate::account_pool_recovery::RecoveryWaitBudget;
use crate::account_pool_recovery::wait_for_recovery;
use crate::config::ConfigBuilder;

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
