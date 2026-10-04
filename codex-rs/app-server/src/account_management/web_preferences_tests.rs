use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn paired_preferences_survive_manager_recreation_and_reject_unpaired_writes()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let state = WebState {
        manager: Arc::clone(&manager),
        token: Arc::new("fixture-preferences".into()),
        origins: Arc::new(vec![origin.clone()]),
        lifecycle: WebLifecycle::new(Instant::now()),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let client = codex_http_client::HttpClientBuilder::new().build_direct()?;
    let url = format!("{origin}/api/preferences");
    let rejected = client
        .post(&url)
        .json(&serde_json::json!({"language":"zh-CN"}))
        .send()
        .await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(manager.preferences(), ManagerPreferences::default());
    let saved = client
        .post(&url)
        .header("x-codex-pool-token", "fixture-preferences")
        .json(&serde_json::json!({"language":"zh-CN"}))
        .send()
        .await?;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        saved.json::<serde_json::Value>().await?,
        serde_json::json!({"language":"zh-CN"})
    );
    let read = client
        .get(&url)
        .header("x-codex-pool-token", "fixture-preferences")
        .send()
        .await?;
    assert_eq!(
        read.json::<serde_json::Value>().await?,
        serde_json::json!({"language":"zh-CN"})
    );
    let foreign = client
        .post(&url)
        .header("x-codex-pool-token", "fixture-preferences")
        .header("origin", "https://foreign.example")
        .json(&serde_json::json!({"language":"en"}))
        .send()
        .await?;
    assert_eq!(foreign.status(), StatusCode::UNAUTHORIZED);
    stop.send(()).unwrap();
    task.await??;
    let restored = AccountManager::new(
        codex_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?,
    );
    assert_eq!(
        restored.preferences(),
        ManagerPreferences {
            language: ManagerLanguage::SimplifiedChinese,
        }
    );
    Ok(())
}
