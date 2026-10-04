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

fn create_config_toml(codex_home: &Path, chatgpt_base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("chatgpt_base_url = \"{chatgpt_base_url}\"\n"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_quota_read_uses_standby_requirements_with_warmup_disabled() -> Result<()> {
    use wiremock::Mock;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let home = TempDir::new()?;
    let server = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&server).await;
    write_pool_fixture(home.path(), "enterprise");
    write_models_cache(home.path()).await?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = '{}'\ncli_auth_credentials_store = 'file'\n[account_pool]\nwindow_warmup = false\n",
            server.uri()
        ),
    )?;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .and(header("authorization", "Bearer access-standby"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requirements_toml":{"enterprise_managed":[{
                "id":"standby-restricted","name":"Standby requirements",
                "contents":"[application.network]\n[application.network.domains]\n'blocked.example' = 'allow'\n"
            }]}
        })))
        .with_priority(1)
        .mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/config/bundle"))
        .and(header("authorization", "Bearer access-selected"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requirements_toml":{"enterprise_managed":[{
                "id":"selected-unrestricted","name":"Selected requirements",
                "contents":"[application.network]\nenabled = false\n"
            }]}
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-standby"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-selected"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type":"enterprise","rate_limit":{"allowed":true,"limit_reached":false,
                "primary_window":{"used_percent":25,"limit_window_seconds":18000,"reset_after_seconds":18000,
                    "reset_at":chrono::Utc::now().timestamp()+18000}}
        })))
        .expect(1)
        .mount(&server).await;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None), ("CODEX_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let id = mcp.send_raw_request("accountPool/read", None).await?;
    let result: codex_app_server_protocol::AccountPoolReadResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
    assert_eq!(result.active_profile_id.as_deref(), Some("selected-acct"));
    let observed: Vec<_> = result
        .accounts
        .iter()
        .map(|account| {
            (
                account.profile_id.as_str(),
                account
                    .rate_limits
                    .primary
                    .as_ref()
                    .map(|window| window.used_percent),
            )
        })
        .collect();
    assert_eq!(
        observed,
        vec![("selected-acct", Some(25.0)), ("standby-acct", None)]
    );
    Ok(())
}

fn write_profile_credentials(codex_home: &Path, id: &str, access_token: &str, plan_type: &str) {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = b64(&serde_json::to_vec(&json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": plan_type,
            "chatgpt_account_id": format!("account-{id}"),
            "chatgpt_user_id": format!("user-{id}"),
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

fn write_pool_fixture(codex_home: &Path, plan_type: &str) {
    write_profile_credentials(codex_home, "selected-acct", "access-selected", plan_type);
    write_profile_credentials(codex_home, "standby-acct", "access-standby", plan_type);
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
    let routing_server = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&routing_server).await;
    create_config_toml(home.path(), &routing_server.uri())?;
    write_models_cache(home.path()).await?;
    write_pool_fixture(home.path(), "pro");

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_warmup_debug_off_is_distinct_from_pool_enabled_and_rejects_now() -> Result<()>
{
    let home = TempDir::new()?;
    let routing_server = wiremock::MockServer::start().await;
    app_test_support::mount_workspace_routing(&routing_server).await;
    create_config_toml(home.path(), &routing_server.uri())?;
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(home.path().join("config.toml"))?
        .write_all(b"\n[account_pool]\nwindow_warmup = false\n")?;
    write_models_cache(home.path()).await?;
    write_pool_fixture(home.path(), "pro");
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .expect(0)
        .mount(&routing_server)
        .await;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    for run_now in [false, true, true] {
        let id = mcp
            .send_raw_request("accountPool/warmupDebug", Some(json!({"runNow":run_now})))
            .await?;
        let response: codex_app_server_protocol::AccountPoolWarmupDebugResponse =
            timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
        assert_eq!(
            (
                response.pool_enabled,
                response.enabled,
                response.task_running,
                response.pass_requested
            ),
            (Some(true), false, false, false)
        );
        assert_eq!(response.accounts.len(), 2);
        assert!(
            response
                .accounts
                .iter()
                .any(|account| account.candidate_reason.as_deref()
                    == Some("automatic warmup is off"))
        );
        assert!(
            !response
                .events
                .iter()
                .any(|event| event.message == "run now requested")
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum StandbyWorkspacePolicy {
    Unrestricted,
    PoolOnly,
    Restricted,
}

#[test_case::test_case(StandbyWorkspacePolicy::Unrestricted; "standby_allows_inference")]
#[test_case::test_case(StandbyWorkspacePolicy::Restricted; "standby_policy_fails_closed")]
#[test_case::test_case(StandbyWorkspacePolicy::PoolOnly; "pool_only_login_loads_foreground_policy_immediately")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_warmup_discovers_the_standby_workspace_without_switching_execution(
    policy: StandbyWorkspacePolicy,
) -> Result<()> {
    use core_test_support::responses;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use tokio::net::TcpListener;
    use tokio::net::TcpStream;
    use tokio_rustls::TlsAcceptor;
    use tokio_rustls::rustls;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    let home = TempDir::new()?;
    let metadata = MockServer::start().await;
    let inference = MockServer::start().await;
    // Workspace discovery requires an HTTPS origin. Terminate TLS locally and forward to
    // wiremock so both the WebSocket handshake and HTTP inference stay in this fixture.
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])?;
    let cert_path = home.path().join("warmup-ca.pem");
    std::fs::write(&cert_path, cert.cert.pem())?;
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
    )?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let routed_origin = format!("https://{}", listener.local_addr()?);
    let inference_address = *inference.address();
    let _tls_bridge = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            connections.spawn(async move {
                let mut client = acceptor.accept(stream).await?;
                let mut upstream = TcpStream::connect(inference_address).await?;
                tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
                Ok::<(), anyhow::Error>(())
            });
        }
    }));
    app_test_support::mount_workspace_routing(&metadata).await;
    let (requirements, expected_outcome, expected_requests) = match policy {
        StandbyWorkspacePolicy::Unrestricted | StandbyWorkspacePolicy::PoolOnly => {
            ("[application.network]\nenabled = false", "succeeded", 1)
        }
        StandbyWorkspacePolicy::Restricted => (
            "[application.network]\n[application.network.domains]\n'blocked.example' = 'allow'",
            "failed",
            0,
        ),
    };
    Mock::given(method("GET")).and(path("/api/codex/config/bundle"))
        .and(header("authorization", "Bearer access-standby"))
        .and(header("chatgpt-account-id", "account-standby-acct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requirements_toml": {"enterprise_managed": [{
                "id": "standby-policy", "name": "Standby workspace policy", "contents": requirements,
            }]}
        }))).mount(&metadata).await;
    Mock::given(method("GET")).and(path("/api/codex/config/bundle"))
        .and(header("authorization", "Bearer access-selected"))
        .and(header("chatgpt-account-id", "account-selected-acct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "requirements_toml": {"enterprise_managed": [{
                "id": "foreground-policy", "name": "Foreground workspace policy",
                "contents": "[application.network]\nenabled = false\n[application.network.domains]\n'foreground.example' = 'allow'",
            }]}
        }))).mount(&metadata).await;
    Mock::given(method("GET"))
        .and(path("/api/codex/accounts/check"))
        .and(header("authorization", "Bearer access-standby"))
        .and(header("chatgpt-account-id", "account-standby-acct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accounts": [{"id": "account-standby-acct", "workspace_backend_origin": routed_origin,
                "account_routing_override": "us"}]
        })))
        .with_priority(1)
        .expect(expected_requests)
        .mount(&metadata)
        .await;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = \"{}\"\nmodel = \"warmup-test-model\"\nopenai_base_url = \"{}/backend-api/codex\"\ncli_auth_credentials_store = 'file'\n",
            metadata.uri(),
            metadata.uri()
        ),
    )?;
    write_pool_fixture(home.path(), "enterprise");
    if !matches!(policy, StandbyWorkspacePolicy::PoolOnly) {
        std::fs::copy(
            home.path().join("auth-profiles/selected-acct/auth.json"),
            home.path().join("auth.json"),
        )?;
    }
    let mut model = codex_models_manager::warmup_models_catalog(/*preferred*/ None)
        .models
        .into_iter()
        .find(|model| model.slug == "gpt-6-astra")
        .expect("fixture model");
    model.slug = "warmup-test-model".into();
    model.use_responses_lite = false;
    let catalog = codex_protocol::openai_models::ModelsResponse {
        models: vec![model.clone()],
    };
    app_test_support::write_models_cache_with_models(home.path(), vec![model]).await?;
    let _models = responses::mount_models_once(&metadata, catalog).await;
    let generated = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&generated);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-standby"))
        .and(header("chatgpt-account-id", "account-standby-acct"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "plan_type": "team",
                "rate_limit": {"allowed":true,"limit_reached":false,
                    "primary_window":{"used_percent":u32::from(observed.load(Ordering::SeqCst)),
                        "limit_window_seconds":18000,"reset_after_seconds":18000,
                        "reset_at":chrono::Utc::now().timestamp()+18000}}
            }))
        })
        .mount(&metadata)
        .await;
    // The built-in provider enables WebSockets; 426 explicitly selects its HTTP fallback.
    Mock::given(method("GET"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer access-standby"))
        .and(header("chatgpt-account-id", "account-standby-acct"))
        .and(header("x-openai-account-routing-override", "us"))
        .respond_with(ResponseTemplate::new(426))
        .expect(expected_requests)
        .mount(&inference)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer access-standby"))
        .and(header("chatgpt-account-id", "account-standby-acct"))
        .and(header("x-openai-account-routing-override", "us"))
        .respond_with(move |_: &wiremock::Request| {
            generated.store(true, Ordering::SeqCst);
            responses::sse_response(responses::sse(vec![
                responses::ev_response_created("warmup-routed"),
                responses::ev_completed("warmup-routed"),
            ]))
        })
        .expect(expected_requests)
        .mount(&inference)
        .await;
    let mut env_overrides = vec![
        ("OPENAI_API_KEY", None),
        ("CODEX_API_KEY", None),
        ("SSL_CERT_FILE", None),
        ("CODEX_CA_CERTIFICATE", cert_path.to_str()),
    ];
    env_overrides.extend(
        codex_network_proxy::PROXY_ENV_KEYS
            .iter()
            .map(|key| (*key, None)),
    );
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&env_overrides)
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let id = mcp.send_config_requirements_read_request().await?;
    let foreground_before: serde_json::Value =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
    assert_eq!(
        foreground_before["requirements"]["application"],
        json!({"network": {
            "enabled": false, "domains": {"foreground.example": "allow"}
        }})
    );
    let id = mcp
        .send_raw_request("accountPool/warmupDebug", Some(json!({"runNow":true})))
        .await?;
    let started: codex_app_server_protocol::AccountPoolWarmupDebugResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
    assert!(started.pass_requested);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let id = mcp
            .send_raw_request("accountPool/warmupDebug", Some(json!({"runNow":false})))
            .await?;
        let result: codex_app_server_protocol::AccountPoolWarmupDebugResponse =
            timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
        if result.accounts.iter().any(|account| {
            account.profile_id == "standby-acct"
                && account.persisted_warmup_outcome.as_deref() == Some(expected_outcome)
        }) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{result:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = metadata
        .received_requests()
        .await
        .expect("metadata requests");
    assert!(requests.iter().any(|request| {
        request.url.path() == "/api/codex/config/bundle"
            && request
                .headers
                .get("authorization")
                .is_some_and(|value| value == "Bearer access-standby")
            && request
                .headers
                .get("chatgpt-account-id")
                .is_some_and(|value| value == "account-standby-acct")
    }));
    assert!(
        !requests
            .iter()
            .any(|request| request.url.path().ends_with("/responses"))
    );
    let inference_requests = inference
        .received_requests()
        .await
        .expect("inference requests");
    assert_eq!(
        inference_requests
            .iter()
            .filter(|request| request.method == wiremock::http::Method::POST
                && request.url.path().ends_with("/responses"))
            .count() as u64,
        expected_requests
    );
    let id = mcp
        .send_raw_request("accountPool/read", Some(json!({})))
        .await?;
    let pool: codex_app_server_protocol::AccountPoolReadResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
    assert_eq!(pool.active_profile_id.as_deref(), Some("selected-acct"));
    let id = mcp.send_config_requirements_read_request().await?;
    let foreground: serde_json::Value =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(id)).await??;
    assert_eq!(foreground, foreground_before);
    metadata.verify().await;
    inference.verify().await;
    Ok(())
}
