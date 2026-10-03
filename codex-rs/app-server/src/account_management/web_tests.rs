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
        lifecycle: WebLifecycle::new(Instant::now()),
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

#[tokio::test]
async fn browser_exit_stops_paired_listener_and_rejects_foreign_shutdown() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let serving = Arc::clone(&manager);
    let (ready, url) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        serve(
            serving,
            AccountManagerWebOptions {
                listen: "127.0.0.1:0".parse().unwrap(),
                allowed_origins: Vec::new(),
            },
            move |url| {
                let _ = ready.send(url.to_string());
            },
        )
        .await
    });
    let url = url::Url::parse(&url.await?)?;
    let token = url.fragment().unwrap().strip_prefix("pair=").unwrap();
    let origin = url.origin().ascii_serialization();
    let client = codex_http_client::HttpClientBuilder::new().build_direct()?;
    let page = client.get(&origin).send().await?.text().await?;
    let exit_button = page
        .split("id=\"stop-manager\"")
        .nth(1)
        .unwrap()
        .split("</button>")
        .next()
        .unwrap();
    let exit_note = page
        .split("id=\"exit-note\"")
        .nth(1)
        .unwrap()
        .split("</p>")
        .next()
        .unwrap();
    insta::assert_snapshot!(
        "webui_exit_controls",
        format!(
            "Stop button: {}\nExit guidance: {}",
            exit_button.split_whitespace().collect::<Vec<_>>().join(" "),
            exit_note.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    );
    let tab = uuid::Uuid::new_v4().to_string();
    let pair = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .json(&serde_json::json!({"token":token,"clientId":tab}))
        .send()
        .await?;
    assert_eq!(pair.status(), StatusCode::OK);
    let unauthorized = client
        .post(format!("{origin}/api/shutdown"))
        .header("x-codex-pool-token", token)
        .header("origin", "https://foreign.example")
        .send()
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let heartbeat = client
        .post(format!("{origin}/api/heartbeat"))
        .header("x-codex-pool-token", token)
        .header("origin", &origin)
        .json(&serde_json::json!({"clientId":tab}))
        .send()
        .await?;
    assert_eq!(heartbeat.status(), StatusCode::OK);
    let stopped = client
        .post(format!("{origin}/api/shutdown"))
        .header("x-codex-pool-token", token)
        .header("origin", &origin)
        .send()
        .await?;
    assert_eq!(
        stopped.json::<serde_json::Value>().await?,
        serde_json::json!({"stopped":true})
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), task).await???;
    assert!(
        manager
            .login_shutdown
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(client.get(&origin).send().await.is_err());
    Ok(())
}

#[tokio::test]
async fn leaving_tab_rejects_delayed_heartbeat_but_accepts_reloaded_tab() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let manager = AccountManager::new(config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let state = WebState {
        manager,
        token: Arc::new("fixture-pair".into()),
        origins: Arc::new(vec![origin.clone()]),
        lifecycle: WebLifecycle::new(Instant::now()),
    };
    let stop = state.lifecycle.shutdown();
    let app = router(state);
    let stopping = stop.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(stopping.cancelled_owned())
            .await
    });
    let client = codex_http_client::HttpClientBuilder::new().build_direct()?;
    let original = uuid::Uuid::new_v4().to_string();
    let reloaded = uuid::Uuid::new_v4().to_string();
    for (path, tab, expected) in [
        ("heartbeat", &original, StatusCode::OK),
        ("leave", &original, StatusCode::OK),
        ("heartbeat", &original, StatusCode::BAD_REQUEST),
        ("heartbeat", &reloaded, StatusCode::OK),
    ] {
        let response = client
            .post(format!("{origin}/api/{path}"))
            .header("x-codex-pool-token", "fixture-pair")
            .header("origin", &origin)
            .json(&serde_json::json!({"clientId":tab}))
            .send()
            .await?;
        assert_eq!(response.status(), expected);
    }
    stop.cancel();
    task.await??;
    Ok(())
}
