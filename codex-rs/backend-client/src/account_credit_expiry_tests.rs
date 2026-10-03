use super::*;
use base64::Engine as _;
use codex_login::AccountProfileStore;
use codex_login::AuthDotJson;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct Fixture {
    _home: tempfile::TempDir,
    pool: Arc<AccountPool>,
    reader: AccountCreditExpiryReader,
    profiles: AccountProfileStore,
    managers: Vec<Arc<AuthManager>>,
    base_url: String,
}

fn synthetic_auth(owner: &str) -> CodexAuth {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({"https://api.openai.com/auth": {
            "chatgpt_user_id": owner, "chatgpt_account_id": "shared-business-workspace",
            "chatgpt_plan_type": "pro",
        }})
        .to_string(),
    );
    CodexAuth::from_external_chatgpt_tokens(
        &format!("e30.{payload}.synthetic"),
        "shared-business-workspace",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap()
}

fn write_auth(home: &std::path::Path, owner: &str) -> CodexAuth {
    let auth = synthetic_auth(owner);
    let stored = AuthDotJson {
        auth_mode: Some(auth.api_auth_mode()),
        openai_api_key: None,
        tokens: Some(auth.get_token_data().unwrap()),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    std::fs::write(home.join("auth.json"), serde_json::to_vec(&stored).unwrap()).unwrap();
    auth
}

impl Fixture {
    fn new(server: &MockServer, count: usize) -> Self {
        let home = tempfile::tempdir().unwrap();
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let pool = Arc::new(AccountPool::new());
        let mut managers = Vec::new();
        for index in 0..count {
            let profile = profiles
                .allocate_profile(Some(format!("Seat {index}")), index as u32)
                .unwrap();
            let auth = write_auth(&profile.credential_home, &format!("seat-{index}"));
            profiles.complete_profile(&profile.id).unwrap();
            let manager =
                AuthManager::from_auth_for_testing_with_home(auth, profile.credential_home.clone());
            pool.register(profile, Arc::clone(&manager)).unwrap();
            managers.push(manager);
        }
        Self {
            reader: AccountCreditExpiryReader::new(home.path().to_path_buf()),
            _home: home,
            pool,
            profiles,
            managers,
            base_url: format!("{}/backend-api", server.uri()),
        }
    }

    async fn collect(&self, cancellation: &CancellationToken) -> Vec<CreditExpirySnapshot> {
        self.reader
            .collect_due(&self.pool, &self.base_url, cancellation)
            .await
    }

    fn make_due(&self) {
        let mut state = self.reader.state.lock().unwrap();
        for entry in state.entries.values_mut() {
            entry.next_check = Instant::now();
        }
    }
}

fn credit(id: &str, hours: i64) -> serde_json::Value {
    json!({
        "id": id, "reset_type": "codex_rate_limits", "status": "available",
        "granted_at": Utc::now().to_rfc3339(),
        "expires_at": (Utc::now() + chrono::Duration::hours(hours)).to_rfc3339(),
        "title": "Full reset", "description": "Weekly and 5-hour windows",
    })
}

fn response_for_owner(request: &wiremock::Request) -> ResponseTemplate {
    let bearer = request
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    let auth = CodexAuth::from_external_chatgpt_tokens(
        bearer.strip_prefix("Bearer ").unwrap(),
        "shared-business-workspace",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap();
    assert_eq!(
        request
            .headers
            .get("chatgpt-account-id")
            .unwrap()
            .to_str()
            .unwrap(),
        "shared-business-workspace"
    );
    ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
        "available_count": 1, "credits": [credit(&auth.get_chatgpt_user_id().unwrap(), 2)],
    }))
}

#[tokio::test]
async fn distinct_business_seats_bind_their_own_bearer_and_reuse_read_only_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(response_for_owner)
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 2);
    let cancellation = CancellationToken::new();
    let first = fixture.collect(&cancellation).await;
    assert_eq!(fixture.collect(&cancellation).await, first);
    assert_eq!(
        first
            .iter()
            .map(|snapshot| (
                snapshot.label.as_str(),
                snapshot.chatgpt_user_id.as_str(),
                snapshot.credits[0].id.as_str(),
                snapshot.fresh,
            ))
            .collect::<Vec<_>>(),
        vec![
            ("Seat 0", "seat-0", "seat-0", true),
            ("Seat 1", "seat-1", "seat-1", true)
        ]
    );
}

#[tokio::test]
async fn later_pass_reaches_fifth_profile_and_concurrent_calls_do_not_duplicate_reads() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(|request: &wiremock::Request| {
            response_for_owner(request).set_delay(Duration::from_millis(40))
        })
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 5);
    let cancellation = CancellationToken::new();
    let (leader, follower) = tokio::join!(
        fixture.collect(&cancellation),
        fixture.collect(&cancellation)
    );
    assert_eq!(leader.len(), 4);
    assert!(follower.is_empty());
    assert_eq!(fixture.collect(&cancellation).await.len(), 5);
}

#[tokio::test]
async fn only_supported_available_future_codex_credits_are_bounded_and_returned() {
    let server = MockServer::start().await;
    let mut unsupported = credit("wrong-scope", 2);
    unsupported["reset_type"] = json!("chatgpt_rate_limits");
    let mut redeemed = credit("redeemed", 2);
    redeemed["status"] = json!("redeemed");
    let mut no_expiry = credit("unknown-expiry", 2);
    no_expiry["expires_at"] = serde_json::Value::Null;
    let mut invalid_expiry = credit("invalid-expiry", 2);
    invalid_expiry["expires_at"] = json!("not-a-date");
    let mut credits = vec![
        unsupported,
        redeemed,
        no_expiry,
        invalid_expiry,
        credit("expired", -1),
        credit("far-future", 25),
        credit("", 2),
        credit(&"x".repeat(257), 2),
    ];
    for index in 0..20 {
        let mut value = credit(&format!("eligible-{index:02}"), 2);
        value["title"] = json!(format!("Title\n{}", "x".repeat(500)));
        credits.push(value);
    }
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "available_count": 20, "credits": credits,
        })))
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let snapshots = fixture.collect(&CancellationToken::new()).await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].credits.len(), 16);
    assert!(snapshots[0].credits.iter().all(|value| {
        value.id.starts_with("eligible-")
            && value.title.as_ref().unwrap().chars().count() <= 160
            && !value.title.as_ref().unwrap().chars().any(char::is_control)
    }));
}

#[tokio::test]
async fn relogin_while_get_is_pending_discards_previous_seat_credits() {
    let server = MockServer::start().await;
    let (started, received) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |request: &wiremock::Request| {
            if let Some(started) = started.lock().unwrap().take() {
                let _ = started.send(());
            }
            response_for_owner(request).set_delay(Duration::from_millis(100))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let cancellation = CancellationToken::new();
    let relogin = async {
        received.await.unwrap();
        let profile = fixture.pool.snapshots()[0].profile.clone();
        write_auth(&profile.credential_home, "different-seat");
        fixture.managers[0].reload().await;
    };
    let (snapshots, ()) = tokio::join!(fixture.collect(&cancellation), relogin);
    assert_eq!(snapshots, Vec::new());
}

#[tokio::test]
async fn stored_relogin_without_manager_reload_never_reuses_old_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(response_for_owner)
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let cancellation = CancellationToken::new();
    assert_eq!(fixture.collect(&cancellation).await.len(), 1);
    let profile = fixture.pool.snapshots()[0].profile.clone();
    write_auth(&profile.credential_home, "different-seat");
    assert_eq!(fixture.collect(&cancellation).await, Vec::new());
}

#[tokio::test]
async fn stale_manifest_disable_suppresses_reads_and_clears_cached_visibility() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(response_for_owner)
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let cancellation = CancellationToken::new();
    assert_eq!(fixture.collect(&cancellation).await.len(), 1);
    let profile = fixture.pool.snapshots()[0].profile.clone();
    fixture
        .profiles
        .update_profile_metadata(
            &profile.id,
            codex_login::AccountProfileMetadataUpdate {
                label: None,
                priority: None,
                disabled: Some(true),
            },
        )
        .unwrap();
    fixture.make_due();
    assert_eq!(fixture.collect(&cancellation).await, Vec::new());
}

#[tokio::test]
async fn refresh_failure_preserves_cache_with_explicit_stale_freshness_and_backoff() {
    let server = MockServer::start().await;
    let first = Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(response_for_owner)
        .mount_as_scoped(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let cancellation = CancellationToken::new();
    let snapshots = fixture.collect(&cancellation).await;
    drop(first);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*status*/ 503))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    fixture.make_due();
    let mut expected = snapshots;
    expected[0].fresh = false;
    assert_eq!(fixture.collect(&cancellation).await, expected);
    assert_eq!(fixture.collect(&cancellation).await, expected);
}

#[tokio::test]
async fn cancellation_cleans_inflight_state_and_never_issues_a_consume_request() {
    let server = MockServer::start().await;
    let (started, received) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |request: &wiremock::Request| {
            if let Some(started) = started.lock().unwrap().take() {
                let _ = started.send(());
            }
            response_for_owner(request).set_delay(Duration::from_secs(5))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let cancellation = CancellationToken::new();
    let cancel = async {
        received.await.unwrap();
        cancellation.cancel();
    };
    let started_at = Instant::now();
    let (snapshots, ()) = tokio::join!(fixture.collect(&cancellation), cancel);
    assert!(started_at.elapsed() < Duration::from_secs(1));
    assert!(snapshots.is_empty());
    assert!(!fixture.reader.state.lock().unwrap().active);
}

struct OwnedRouting(codex_login::WorkspaceMaintenanceClients);

impl codex_login::WorkspaceRoutingResolver for OwnedRouting {
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
        Box::pin(async { Ok(Some(self.0.clone())) })
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
async fn maintenance_uses_owner_destination_and_policy_instead_of_default_transport() {
    let default = MockServer::start().await;
    let owned = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&default)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(response_for_owner)
        .expect(/*requests*/ 1)
        .mount(&owned)
        .await;
    let fixture = Fixture::new(&default, /*count*/ 1);
    let owner: Arc<dyn codex_login::WorkspaceRoutingResolver> =
        Arc::new(OwnedRouting(codex_login::WorkspaceMaintenanceClients {
            chatgpt_base_url: format!("{}/backend-api", owned.uri()),
            http_client_factory: codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            ),
        }));
    fixture.managers[0].set_workspace_routing_resolver(Arc::downgrade(&owner));
    assert_eq!(fixture.collect(&CancellationToken::new()).await.len(), 1);
    let denied = Fixture::new(&default, /*count*/ 1);
    let unavailable = codex_http_client::NetworkPolicyController::default();
    let denied_owner: Arc<dyn codex_login::WorkspaceRoutingResolver> =
        Arc::new(OwnedRouting(codex_login::WorkspaceMaintenanceClients {
            chatgpt_base_url: format!("{}/backend-api", owned.uri()),
            http_client_factory: codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            )
            .with_network_policy(unavailable.policy()),
        }));
    denied.managers[0].set_workspace_routing_resolver(Arc::downgrade(&denied_owner));
    assert_eq!(denied.collect(&CancellationToken::new()).await, Vec::new());
}

#[tokio::test]
async fn disabled_and_api_key_profiles_never_contact_credit_backend() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 2);
    let profiles = fixture.pool.snapshots();
    fixture
        .pool
        .set_disabled(&profiles[0].profile.id, /*disabled*/ true)
        .unwrap();
    std::fs::write(
        profiles[1].profile.credential_home.join("auth.json"),
        serde_json::to_vec(&json!({
            "OPENAI_API_KEY": "synthetic-key",
        }))
        .unwrap(),
    )
    .unwrap();
    fixture.managers[1].reload().await;
    assert_eq!(fixture.collect(&CancellationToken::new()).await, Vec::new());
}

#[tokio::test]
async fn voucher_extensions_beyond_notice_window_remain_observable_without_prompt_or_post() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
            "available_count": 1, "credits": [credit("extended-voucher", 30 * 24)]
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = Fixture::new(&server, /*count*/ 1);
    let observed = fixture.collect(&CancellationToken::new()).await;
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].credits[0].id, "extended-voucher");
    assert!(observed[0].credits[0].expires_at - Utc::now().timestamp() > 24 * 3_600);
    assert_eq!(fixture.collect(&CancellationToken::new()).await, observed);
}
