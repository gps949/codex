use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use codex_config::AutoResetCredits;
use codex_core::ExecutionAccountPoolHandle;
use codex_core::TurnInputRequest;
use codex_login::AccountAvailability;
use codex_login::AccountAvailabilityMutation;
use codex_login::AccountQuotaEvidence;
use codex_login::AccountRuntimeStateStore;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::write_profile_credentials;

#[expect(clippy::unwrap_used)]
fn write_pool(home: &Path, count: usize) {
    let mut profiles = Vec::new();
    for index in 0..count {
        let id = format!("seat-{index}");
        write_profile_credentials(home, &id, &format!("access-{id}"));
        let path = home.join("auth-profiles").join(&id).join("auth.json");
        let mut auth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let parts: Vec<_> = auth["tokens"]["id_token"]
            .as_str()
            .unwrap()
            .split('.')
            .collect();
        let mut claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        claims["https://api.openai.com/auth"]["chatgpt_account_id"] = json!("shared-workspace");
        claims["https://api.openai.com/auth"]["chatgpt_plan_type"] = json!("team");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("{}.{payload}.{}", parts[0], parts[2]);
        auth["tokens"]["id_token"] = json!(token);
        auth["tokens"]["account_id"] = json!("shared-workspace");
        std::fs::write(path, serde_json::to_vec(&auth).unwrap()).unwrap();
        profiles.push(json!({"id": id, "label": id, "priority": index,
            "credential_location": "managed_profile", "state": "ready", "disabled": false}));
    }
    std::fs::write(
        home.join("account-profiles.json"),
        serde_json::to_vec(&json!({
            "version": 1, "profiles": profiles,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn refusal() -> ResponseTemplate {
    ResponseTemplate::new(/*status*/ 429).set_body_json(json!({"error": {
        "type": "usage_limit_reached", "message": "synthetic quota refusal",
        "resets_at": (chrono::Utc::now() + chrono::Duration::hours(2)).timestamp(),
    }}))
}

fn recovered() -> ResponseTemplate {
    responses::sse_response(sse(vec![
        ev_response_created("recovered"),
        ev_assistant_message("recovered-message", "completed on the recovered seat"),
        ev_completed("recovered"),
    ]))
}

fn usage(request: &wiremock::Request, recovered_seat: &str) -> ResponseTemplate {
    let authorization = request
        .headers
        .get("authorization")
        .expect("bound auth")
        .to_str()
        .expect("synthetic bearer");
    let id = authorization
        .strip_prefix("Bearer access-")
        .expect("synthetic seat");
    let allowed = id == recovered_seat;
    let reset = (chrono::Utc::now() + chrono::Duration::hours(2)).timestamp();
    ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
        "plan_type": "team", "account_id": "shared-workspace", "user_id": format!("user-{id}"),
        "rate_limit": {"allowed": allowed, "limit_reached": !allowed,
            "primary_window": {"used_percent": if allowed { 0 } else { 100 }, "limit_window_seconds": 18000,
                "reset_after_seconds": 7200, "reset_at": reset},
            "secondary_window": {"used_percent": if allowed { 0 } else { 100 }, "limit_window_seconds": 604800,
                "reset_after_seconds": 7200, "reset_at": reset}},
    }))
}

async fn events(thread: &codex_core::CodexThread) -> anyhow::Result<Vec<EventMsg>> {
    let mut events = Vec::new();
    loop {
        let event = thread.next_event().await?.msg;
        let done = matches!(event, EventMsg::TurnComplete(_));
        events.push(event);
        if done {
            return Ok(events);
        }
    }
}

async fn begin(thread: &codex_core::CodexThread) -> anyhow::Result<()> {
    thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "finish within this user turn".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_exhaustion_recovers_the_fifth_business_seat_without_another_message()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let requests = mount_response_sequence(
        &server,
        vec![
            refusal(),
            refusal(),
            refusal(),
            refusal(),
            refusal(),
            recovered(),
        ],
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| usage(request, "seat-4"))
        .expect(/*requests*/ 5)
        .mount(&server)
        .await;
    let base = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(|home| write_pool(home, /*count*/ 5))
        .with_config(move |config| {
            config.chatgpt_base_url = base;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
            config.account_pool.auto_reset_credits = Some(AutoResetCredits::Never);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    begin(&fixture.codex).await?;
    let events = tokio::time::timeout(Duration::from_secs(10), events(&fixture.codex)).await??;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_))),
        "{events:?}"
    );
    let requests = requests.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer access-seat-0".into()),
            Some("Bearer access-seat-1".into()),
            Some("Bearer access-seat-2".into()),
            Some("Bearer access-seat-3".into()),
            Some("Bearer access-seat-4".into()),
            Some("Bearer access-seat-4".into())
        ]
    );
    assert!(
        !server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .any(|request| request.url.path().contains("/consume"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_available_metadata_does_not_loop_on_inference_refusals() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(refusal())
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| usage(request, "seat-0"))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let base = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(|home| write_pool(home, /*count*/ 1))
        .with_config(move |config| {
            config.chatgpt_base_url = base;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    begin(&fixture.codex).await?;
    let events = tokio::time::timeout(Duration::from_secs(10), events(&fixture.codex)).await??;
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                EventMsg::Error(error) => error.codex_error_info.clone(),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![CodexErrorInfo::UsageLimitExceeded]
    );
    assert!(events.iter().any(|event| matches!(event, EventMsg::Warning(warning) if warning.message.contains("0/1 accounts"))));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn earlier_seat_reenters_after_fresh_quota_recovery_and_ignores_its_old_refusal()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let last_started = Arc::new(Notify::new());
    let observed = Arc::clone(&last_started);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-seat-2"))
        .respond_with(move |_: &wiremock::Request| {
            observed.notify_one();
            refusal().set_delay(Duration::from_secs(2))
        })
        .with_priority(1)
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let requests = mount_response_sequence(&server, vec![refusal(), refusal(), recovered()]).await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(|request: &wiremock::Request| usage(request, "seat-0"))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let base = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(|home| write_pool(home, /*count*/ 3))
        .with_config(move |config| {
            config.chatgpt_base_url = base;
            config.account_pool.window_warmup = Some(false);
            config.account_pool.resume_after_reset = Some(false);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let handle = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    handle.ensure_from_config(&fixture.config).await?;
    let pool = handle.account_pool().expect("pool");
    let old = pool.lease()?;
    begin(&fixture.codex).await?;
    tokio::time::timeout(Duration::from_secs(5), last_started.notified()).await?;
    // A fresh seat-bound usage read restores the earlier seat while C's request is in flight.
    // Raw cache edits must never shorten the live cooldown without new recovery evidence.
    let store = AccountRuntimeStateStore::new(fixture.config.codex_home.to_path_buf());
    store.synchronize(&pool)?;
    let (auth, factory) = old
        .auth_manager()
        .auth_with_http_client_factory()
        .await
        .expect("first seat auth");
    let probe = store
        .capture_quota_probe(&pool, &old.profile().id, &auth)?
        .expect("first seat probe");
    let client = codex_backend_client::Client::from_auth(
        fixture.config.chatgpt_base_url.clone(),
        &auth,
        factory,
    );
    let observed = client.get_rate_limits_with_reset_credits().await?;
    assert!(store.reconcile_quota_probe(
        &pool,
        probe,
        AccountQuotaEvidence {
            rate_limits: &observed.rate_limits,
            ordinary_usage_allowed: observed.ordinary_usage_allowed,
            account_id: observed.account_id.as_deref(),
            user_id: observed.user_id.as_deref(),
        }
    )?);
    let events = tokio::time::timeout(Duration::from_secs(10), events(&fixture.codex)).await??;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_))),
        "{events:?}"
    );
    let current = pool.lease()?;
    assert_eq!(current.profile().id, old.profile().id);
    assert!(current.generation() > old.generation());
    assert!(matches!(
        pool.mark_exhausted(&old, Some(chrono::Utc::now() + chrono::Duration::days(1)))?,
        AccountAvailabilityMutation::StaleIgnored { .. }
    ));
    assert_eq!(
        pool.snapshots()
            .into_iter()
            .find(|snapshot| snapshot.profile.id == old.profile().id)
            .expect("first seat")
            .availability,
        AccountAvailability::Available
    );
    assert_eq!(
        requests
            .requests()
            .iter()
            .map(|request| request.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer access-seat-0".into()),
            Some("Bearer access-seat-1".into()),
            Some("Bearer access-seat-0".into())
        ]
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_drops_final_recovery_without_another_inference_request() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let started = Arc::new(Notify::new());
    let observed = Arc::clone(&started);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(refusal())
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(move |request: &wiremock::Request| {
            observed.notify_one();
            usage(request, "seat-0").set_delay(Duration::from_secs(20))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let base = format!("{}/backend-api", server.uri());
    let mut builder = test_codex()
        .without_auth()
        .with_pre_build_hook(|home| write_pool(home, /*count*/ 1))
        .with_config(move |config| {
            config.chatgpt_base_url = base;
            config.account_pool.window_warmup = Some(false);
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    begin(&fixture.codex).await?;
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await?;
    fixture.codex.submit(Op::Interrupt).await?;
    tokio::time::timeout(
        Duration::from_secs(2),
        wait_for_event(&fixture.codex, |event| {
            matches!(event, EventMsg::TurnAborted(_))
        }),
    )
    .await?;
    Ok(())
}
