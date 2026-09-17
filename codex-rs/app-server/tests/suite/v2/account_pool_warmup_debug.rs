use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use base64::Engine as _;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn create_config_toml(codex_home: &Path) -> std::io::Result<()> {
    std::fs::write(codex_home.join("config.toml"), "")
}

fn write_profile_credentials(codex_home: &Path, id: &str, access_token: &str) {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64(&serde_json::to_vec(&json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": format!("account-{id}"),
        }
    }))
    .expect("payload"));
    let fake_jwt = format!("{header}.{payload}.{}", b64(b"sig"));

    let credential_home = codex_home.join("auth-profiles").join(id);
    std::fs::create_dir_all(&credential_home).expect("credential home");
    std::fs::write(
        credential_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": access_token,
                "refresh_token": format!("refresh-{id}"),
                "account_id": format!("account-{id}"),
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .expect("auth.json"),
    )
    .expect("write auth.json");
}

fn write_pool_fixture(codex_home: &Path) {
    write_profile_credentials(codex_home, "selected-acct", "access-selected");
    write_profile_credentials(codex_home, "standby-acct", "access-standby");
    std::fs::write(
        codex_home.join("account-profiles.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "profiles": [
                {
                    "id": "selected-acct",
                    "label": null,
                    "priority": 10,
                    "credential_location": "managed_profile",
                    "state": "ready",
                    "disabled": false,
                },
                {
                    "id": "standby-acct",
                    "label": null,
                    "priority": 20,
                    "credential_location": "managed_profile",
                    "state": "ready",
                    "disabled": false,
                }
            ],
        }))
        .expect("manifest"),
    )
    .expect("write manifest");
    std::fs::write(
        codex_home.join("account-runtime-state.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "active_profile_id": "selected-acct",
            "profiles": [],
        }))
        .expect("runtime state"),
    )
    .expect("write runtime state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_warmup_debug_lists_candidates_and_task() -> Result<()> {
    let home = TempDir::new()?;
    create_config_toml(home.path())?;
    write_models_cache(home.path())?;
    write_pool_fixture(home.path());

    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;

    let id = mcp
        .send_raw_request("accountPool/warmupDebug", Some(json!({ "runNow": false })))
        .await?;
    let response: codex_app_server_protocol::AccountPoolWarmupDebugResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;

    assert!(response.enabled);
    assert!(response.task_running);
    assert!(!response.pass_requested);
    assert_eq!(response.interval_seconds, 300);
    assert_eq!(response.settle_seconds, 30);
    assert_eq!(response.rotation_strategy, "fillFirst");
    assert_eq!(response.accounts.len(), 2);
    let standby = response
        .accounts
        .iter()
        .find(|account| account.profile_id == "standby-acct")
        .expect("standby");
    assert!(standby.is_candidate);
    assert!(!standby.is_active);
    let current = response
        .accounts
        .iter()
        .find(|account| account.profile_id == "selected-acct")
        .expect("current");
    assert!(current.is_active);
    assert!(!current.is_candidate);
    assert!(
        response
            .events
            .iter()
            .any(|event| event.message == "task spawned"),
        "expected task spawned event, got {:?}",
        response.events
    );
    Ok(())
}
