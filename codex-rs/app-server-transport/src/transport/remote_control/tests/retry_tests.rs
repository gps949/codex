//! Exercises overload retry deadlines over HTTP and WebSocket connections.
//! Auth reloads must respect the deadline; shutdown must remain prompt.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn rate_limited_enrollment_respects_retry_after() {
    assert_overload_retry_after("429 Too Many Requests", /*reject_enrollment*/ true).await;
}

#[tokio::test]
async fn rate_limited_websocket_respects_retry_after() {
    assert_overload_retry_after("429 Too Many Requests", /*reject_enrollment*/ false).await;
}

#[tokio::test]
async fn enrollment_resumes_after_retry_after() {
    assert_overload_retry_after("503 Service Unavailable", /*reject_enrollment*/ true).await;
}

#[tokio::test]
async fn websocket_resumes_after_retry_after() {
    assert_overload_retry_after("503 Service Unavailable", /*reject_enrollment*/ false).await;
}

async fn assert_overload_retry_after(status: &str, reject_enrollment: bool) {
    let verify_recovery = status == "503 Service Unavailable";
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    let (transport_event_tx, _transport_event_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let shutdown_token = CancellationToken::new();
    let auth_manager = remote_control_auth_manager_with_home(&codex_home);
    let mut initial_auth = remote_control_auth_dot_json(Some("account_id"));
    initial_auth
        .tokens
        .as_mut()
        .expect("fixture should contain tokens")
        .access_token = "Initial Access Token".to_string();
    save_auth(
        codex_home.path(),
        &initial_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("initial credentials should save");
    auth_manager.reload().await;
    let (remote_task, remote_handle) = start_remote_control(
        RemoteControlStartConfig {
            remote_control_url: remote_control_url_for_listener(&listener),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            policy: RemoteControlPolicy::Allowed,
        },
        Some(remote_control_state_runtime(&codex_home).await),
        auth_manager.clone(),
        transport_event_tx,
        shutdown_token.clone(),
        /*app_server_client_name_rx*/ None,
        RemoteControlStartupMode::EnabledEphemeral,
    )
    .await
    .expect("remote control should start");
    let mut status_rx = remote_handle.status_receiver();
    let mut rejected_request = accept_http_request(&listener).await;
    assert_eq!(
        rejected_request.request_line,
        "POST /backend-api/wham/remote/control/server/enroll HTTP/1.1"
    );
    if !reject_enrollment {
        respond_with_json(
            rejected_request.stream,
            remote_control_server_token_response(
                "srv_e_test",
                "env_test",
                TEST_REMOTE_CONTROL_SERVER_TOKEN,
            ),
        )
        .await;
        rejected_request = accept_http_request(&listener).await;
        assert_eq!(
            rejected_request.request_line,
            "GET /backend-api/wham/remote/control/server HTTP/1.1"
        );
    }
    let response_started_at = std::time::Instant::now();
    respond_with_status_and_headers(
        rejected_request.stream,
        status,
        &[("Retry-After", if verify_recovery { "3" } else { "120" })],
        "overloaded",
    )
    .await;
    timeout(
        Duration::from_secs(5),
        status_rx.wait_for(|status| status.status == RemoteControlConnectionStatus::Errored),
    )
    .await
    .expect("the overload response should be processed")
    .expect("the status channel should remain open");

    let auth_changes = auth_manager.auth_change_receiver();
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json(Some("account_id")),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("updated credentials should save");
    auth_manager.reload().await;
    assert!(
        auth_changes
            .has_changed()
            .expect("auth watch should remain open")
    );

    if !verify_recovery {
        let pairing_error = timeout(
            Duration::from_secs(1),
            remote_handle.start_pairing(
                RemoteControlPairingStartParams::default(),
                /*app_server_client_name*/ None,
            ),
        )
        .await
        .expect("pairing should report the active retry delay promptly")
        .expect_err("pairing must respect the shared server retry delay");
        let retry_delay = server_api::remote_control_retry_delay(&pairing_error)
            .expect("pairing should preserve the overload retry deadline");
        assert!(
            retry_delay >= Duration::from_secs(120).saturating_sub(response_started_at.elapsed()),
            "pairing must preserve the full remaining Retry-After interval"
        );
        assert!(
            timeout(Duration::from_secs(2), listener.accept())
                .await
                .is_err(),
            "no enrollment, refresh or handshake should retry before Retry-After ({status})"
        );
        remote_handle.disable_ephemeral().await;
        assert!(
            timeout(Duration::from_secs(2), listener.accept())
                .await
                .is_err(),
            "disabling remote control must stop connection attempts"
        );
        remote_handle
            .enable_ephemeral()
            .expect("remote control should enable again");
        assert!(
            timeout(Duration::from_secs(2), listener.accept())
                .await
                .is_err(),
            "re-enabling remote control must preserve the server retry delay"
        );
    }
    if !reject_enrollment {
        assert_eq!(
            remote_handle
                .inner
                .session()
                .current_enrollment
                .snapshot()
                .and_then(|enrollment| enrollment.remote_control_token),
            Some(TEST_REMOTE_CONTROL_SERVER_TOKEN.to_string()),
            "overload must preserve the enrolled token"
        );
    }
    if verify_recovery {
        remote_handle.disable_ephemeral().await;
        remote_handle
            .enable_ephemeral()
            .expect("remote control should enable again");
        let (stream, _) = timeout(Duration::from_secs(45), listener.accept())
            .await
            .expect("retry should resume after the server delay and bounded jitter")
            .expect("retry connection should succeed");
        assert!(
            response_started_at.elapsed() >= Duration::from_secs(3),
            "the retry must wait for the full Retry-After interval"
        );
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        timeout(Duration::from_secs(5), reader.read_line(&mut request_line))
            .await
            .expect("retry should send the request promptly")
            .expect("retry request should read");
        assert_eq!(
            request_line.trim_end(),
            if reject_enrollment {
                "POST /backend-api/wham/remote/control/server/enroll HTTP/1.1"
            } else {
                "GET /backend-api/wham/remote/control/server HTTP/1.1"
            }
        );
    }
    shutdown_token.cancel();
    timeout(Duration::from_secs(1), remote_task)
        .await
        .expect("shutdown must interrupt the server retry delay")
        .expect("remote task should finish");
}

struct InitiallyUnavailablePolicy {
    controller: codex_http_client::NetworkPolicyController,
    attempts: std::sync::atomic::AtomicUsize,
}

impl codex_login::PrimaryLoginPolicyLoader for InitiallyUnavailablePolicy {
    fn prepare(&self, _manager: Arc<AuthManager>) -> codex_login::ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            let revision = self.controller.policy().revision();
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                == 0
            {
                self.controller.unavailable(revision);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "synthetic initial policy timeout",
                ));
            }
            assert!(
                self.controller
                    .publish(revision, codex_http_client::DestinationPolicy::Unrestricted,)
            );
            Ok(())
        })
    }
}

#[tokio::test]
async fn host_policy_retry_recovers_without_reenabling_or_changing_owner() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let codex_home = TempDir::new().expect("temp dir should create");
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json(Some("account_id")),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("initial credentials should save");
    let policy = Arc::new(InitiallyUnavailablePolicy {
        controller: Default::default(),
        attempts: std::sync::atomic::AtomicUsize::new(0),
    });
    let factory = codex_login::test_support::transport_default_auth_route_config()
        .http_client_factory()
        .clone()
        .with_network_policy(policy.controller.policy());
    let runtime = codex_login::PrimaryLoginRuntime::start(codex_login::AuthConfig {
        codex_home: codex_home.path().to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_login_method: None,
        chatgpt_base_url: None,
        forced_chatgpt_workspace_id: None,
        managed_auth_policy: codex_config::ManagedAuthPolicy::default(),
        auth_route_config: codex_login::AuthRouteConfig::from_http_client_factory(factory),
    })
    .await
    .expect("primary-login runtime should start");
    let loader: Arc<dyn codex_login::PrimaryLoginPolicyLoader> = policy.clone();
    runtime.set_policy_loader(Arc::downgrade(&loader));
    let policy_changes = policy
        .controller
        .policy()
        .changes()
        .expect("managed policy should have a changes receiver");
    let manager = runtime.auth_manager();
    let changes = manager.auth_change_state_receiver();
    let owner_generation = changes.borrow().owner_generation;
    let (transport_event_tx, _transport_event_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let shutdown_token = CancellationToken::new();
    let (remote_task, remote_handle) = start_remote_control(
        RemoteControlStartConfig {
            remote_control_url: remote_control_url_for_listener(&listener),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            policy: RemoteControlPolicy::Allowed,
        },
        Some(remote_control_state_runtime(&codex_home).await),
        manager,
        transport_event_tx,
        shutdown_token.clone(),
        /*app_server_client_name_rx*/ None,
        RemoteControlStartupMode::EnabledEphemeral,
    )
    .await
    .expect("remote control should start");
    let mut status_rx = remote_handle.status_receiver();
    timeout(
        Duration::from_secs(2),
        status_rx.wait_for(|status| status.status == RemoteControlConnectionStatus::Errored),
    )
    .await
    .expect("initial unavailable policy should be processed promptly")
    .expect("status receiver should remain open");
    assert!(
        timeout(Duration::from_secs(1), listener.accept())
            .await
            .is_err(),
        "unavailable policy must prevent backend contact and avoid immediate retries"
    );
    assert_eq!(
        policy.attempts.load(std::sync::atomic::Ordering::Acquire),
        1
    );
    assert_eq!(changes.borrow().owner_generation, owner_generation);
    assert!(
        !policy_changes
            .has_changed()
            .expect("policy receiver should remain open"),
        "the first failed load must exercise the absence of a policy-change notification"
    );

    let enroll_request = accept_http_request(&listener).await;
    assert_eq!(
        enroll_request.request_line,
        "POST /backend-api/wham/remote/control/server/enroll HTTP/1.1"
    );
    assert_eq!(
        enroll_request.headers.get("chatgpt-account-id"),
        Some(&"account_id".to_string())
    );
    respond_with_json(
        enroll_request.stream,
        remote_control_server_token_response(
            "srv_e_test",
            "env_test",
            TEST_REMOTE_CONTROL_SERVER_TOKEN,
        ),
    )
    .await;
    let (_handshake_request, _websocket) =
        accept_remote_control_backend_connection(&listener).await;
    timeout(
        Duration::from_secs(2),
        status_rx.wait_for(|status| status.status == RemoteControlConnectionStatus::Connected),
    )
    .await
    .expect("remote control should reconnect after its policy retry")
    .expect("status receiver should remain open");
    assert_eq!(changes.borrow().owner_generation, owner_generation);

    shutdown_token.cancel();
    timeout(Duration::from_secs(1), remote_task)
        .await
        .expect("shutdown should finish promptly")
        .expect("remote task should finish");
}
