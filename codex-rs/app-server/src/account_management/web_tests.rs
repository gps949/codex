use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn paired_manager_http_rejects_foreign_origin_and_covers_account_operations()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let record = manager
        .store()
        .allocate_profile(Some("Pending".into()), /*priority*/ 10)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let state = WebState {
        manager,
        token: Arc::new("fixture-pair".into()),
        origins: Arc::new(vec![origin.clone()]),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let app = router(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let client = codex_http_client::HttpClientBuilder::new().build_direct()?;
    let unauthenticated = client.get(format!("{origin}/api/inventory")).send().await?;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let foreign = client
        .post(format!("{origin}/api/session"))
        .header("origin", "https://foreign.example")
        .json(&serde_json::json!({"token":"fixture-pair"}))
        .send()
        .await?;
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    let pair = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .json(&serde_json::json!({"token":"fixture-pair"}))
        .send()
        .await?;
    assert_eq!(pair.status(), StatusCode::OK);
    assert!(!pair.headers().contains_key("set-cookie"));
    let pair: serde_json::Value = pair.json().await?;
    let session = pair["sessionToken"].as_str().unwrap();
    let read = client
        .get(format!("{origin}/api/inventory"))
        .header("x-codex-pool-token", session)
        .send()
        .await?;
    assert_eq!(
        (read.status(), read.headers()["cache-control"].to_str()?),
        (StatusCode::OK, "no-store")
    );
    let inventory: serde_json::Value = read.json().await?;
    assert_eq!(inventory["accounts"][0]["loginState"], "pending");
    for (payload, status) in [
        (
            serde_json::json!({"type":"use","profileId":record.id}),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"type":"update","profileId":record.id,"label":"Renamed","disabled":true}),
            StatusCode::OK,
        ),
        (serde_json::json!({"type":"automatic"}), StatusCode::OK),
        (
            serde_json::json!({"type":"remove","profileId":record.id,"keepCredentials":true}),
            StatusCode::OK,
        ),
    ] {
        let response = client
            .post(format!("{origin}/api/operation"))
            .header("x-codex-pool-token", session)
            .header("origin", &origin)
            .json(&payload)
            .send()
            .await?;
        assert_eq!(response.status(), status);
    }
    let unauthorized = client
        .post(format!("{origin}/api/operation"))
        .header("x-codex-pool-token", session)
        .header("origin", "https://foreign.example")
        .json(&serde_json::json!({"type":"automatic"}))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    stop.send(()).unwrap();
    task.await??;
    Ok(())
}
