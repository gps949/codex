use super::*;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::session::tests::make_session_and_context_with_auth_and_config_and_rx;
use base64::Engine as _;
use codex_backend_client::ExpiringResetCredit;
use codex_login::AccountProfileStore;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct Fixture {
    home: tempfile::TempDir,
    config: Config,
    execution: Arc<ExecutionAuth>,
    pool: Arc<AccountPool>,
    coordinator: Arc<AccountCreditExpiryReminders>,
    snapshot: CreditExpirySnapshot,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let profile =
            profiles.allocate_profile(Some("Business seat".into()), /*priority*/ 0)?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({
            "https://api.openai.com/auth": {
                "chatgpt_user_id": "synthetic-seat", "chatgpt_account_id": "synthetic-workspace",
                "chatgpt_plan_type": "pro"
            }
        }).to_string());
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&json!({
                "tokens": {
                    "id_token": format!("e30.{payload}.synthetic"),
                    "access_token": "synthetic-access", "refresh_token": "synthetic-refresh",
                    "account_id": "synthetic-workspace"
                },
                "last_refresh": Utc::now()
            }))?,
        )?;
        profiles.complete_profile(&profile.id)?;
        let mut config = ConfigBuilder::without_managed_config_for_tests()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        config.account_pool.window_warmup = Some(false);
        let execution = Arc::new(ExecutionAuth::legacy(AuthManager::from_auth_for_testing(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        )));
        execution.ensure_runtime_from_config(&config).await?;
        let pool = execution.account_pool().expect("installed pool");
        let coordinator = Arc::new(AccountCreditExpiryReminders::new(home.path().to_path_buf()));
        execution
            .credit_expiry_reminders
            .set(Arc::clone(&coordinator))
            .ok()
            .expect("new coordinator");
        let snapshot = CreditExpirySnapshot {
            profile_id: profile.id,
            label: "Business seat".into(),
            account_id: "synthetic-workspace".into(),
            chatgpt_user_id: "synthetic-seat".into(),
            observed_at: Utc::now().timestamp(),
            fresh: true,
            credits: vec![ExpiringResetCredit {
                id: "voucher-1".into(),
                title: Some("Full reset".into()),
                description: None,
                expires_at: Utc::now().timestamp() + 4 * 3_600,
            }],
        };
        Ok(Self {
            home,
            config,
            execution,
            pool,
            coordinator,
            snapshot,
        })
    }

    fn warning(
        &self,
        snapshots: &[CreditExpirySnapshot],
        cancellation: &CancellationToken,
    ) -> Option<WarningEvent> {
        self.coordinator
            .claim_warning(&self.execution, &self.pool, snapshots, cancellation)
    }

    async fn session(
        &self,
        base_url: String,
    ) -> (
        Arc<Session>,
        Arc<TurnContext>,
        async_channel::Receiver<codex_protocol::protocol::Event>,
    ) {
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            Vec::new(),
            |config| {
                config.codex_home =
                    codex_utils_absolute_path::AbsolutePathBuf::try_from(self.home.path())
                        .expect("absolute test home");
                config.chatgpt_base_url = base_url;
                config.account_pool.window_warmup = Some(false);
            },
        )
        .await
    }
}

fn user_input() -> Vec<TurnInput> {
    vec![TurnInput::UserInput {
        content: vec![UserInput::Text {
            text: "Continue my task".into(),
            text_elements: Vec::new(),
        }],
        client_id: None,
        metadata: Default::default(),
    }]
}

#[tokio::test]
async fn credit_expiry_notice_groups_duplicate_profiles_and_survives_restart() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    let cancellation = CancellationToken::new();
    let snapshots = vec![fixture.snapshot.clone(), fixture.snapshot.clone()];
    let warning = fixture
        .warning(&snapshots, &cancellation)
        .expect("fresh notice");
    insta::assert_snapshot!(warning.message, @"Codex reset credit (Full reset) for account `Business seat` expires in 4h 00m. Review credits on the host with `codex account manage`. In mobile Remote, `/account manage` shows the account overview. A reset clears existing usage windows; it does not add quota. No credit was used.");
    assert!(fixture.warning(&snapshots, &cancellation).is_none());
    let restarted = AccountCreditExpiryReminders::new(fixture.home.path().to_path_buf());
    assert!(
        restarted
            .claim_warning(&fixture.execution, &fixture.pool, &snapshots, &cancellation)
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn credit_expiry_notice_rejects_stale_cancelled_and_replaced_seats() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let mut stale = fixture.snapshot.clone();
    stale.fresh = false;
    assert!(
        fixture
            .warning(&[stale], &CancellationToken::new())
            .is_none()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        fixture
            .warning(std::slice::from_ref(&fixture.snapshot), &cancelled)
            .is_none()
    );
    let mut foreign = fixture.snapshot.clone();
    foreign.chatgpt_user_id = "replacement-seat".into();
    assert!(
        fixture
            .warning(&[foreign], &CancellationToken::new())
            .is_none()
    );
    assert!(
        !fixture
            .home
            .path()
            .join(".credit-expiry-reminders.json")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn credit_expiry_notice_remains_available_when_every_seat_is_exhausted() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    let lease = fixture.pool.lease()?;
    fixture
        .pool
        .mark_exhausted(&lease, Some(Utc::now() + chrono::Duration::days(2)))?;
    assert!(fixture.execution.active_lease().is_none());
    let before = fixture.pool.snapshots();
    assert!(
        fixture
            .warning(
                std::slice::from_ref(&fixture.snapshot),
                &CancellationToken::new()
            )
            .is_some()
    );
    assert_eq!(fixture.pool.snapshots(), before);
    assert!(fixture.execution.active_lease().is_none());
    Ok(())
}

#[tokio::test]
async fn credit_expiry_notice_rotates_bounded_claim_batches() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let mut snapshot = fixture.snapshot.clone();
    snapshot.credits = (0..=MAX_CLAIM_ATTEMPTS)
        .map(|index| ExpiringResetCredit {
            id: format!("voucher-{index:02}"),
            title: Some(format!("Credit {index}")),
            description: None,
            expires_at: snapshot.credits[0].expires_at + index as i64,
        })
        .collect();
    for credit in snapshot.credits.iter().take(MAX_CLAIM_ATTEMPTS) {
        fixture.coordinator.store.try_claim(
            &CreditExpiryCandidate {
                account_id: &snapshot.account_id,
                chatgpt_user_id: &snapshot.chatgpt_user_id,
                credit_id: &credit.id,
                reset_type: "codex_rate_limits",
                status: "available",
                expires_at: credit.expires_at,
            },
            Utc::now().timestamp(),
        )?;
    }
    assert!(
        fixture
            .warning(&[snapshot.clone()], &CancellationToken::new())
            .is_none()
    );
    let warning = fixture
        .warning(&[snapshot], &CancellationToken::new())
        .expect("next bounded batch");
    assert!(warning.message.contains("Credit 16"));
    assert!(warning.message.contains("16 other expiring credit(s)"));
    Ok(())
}

#[tokio::test]
async fn completed_credit_expiry_turn_keeps_cache_without_late_warning_or_model_context()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({
                    "available_count": 1, "credits": [{
                        "id": "voucher-1", "reset_type": "codex_rate_limits", "status": "available",
                        "granted_at": Utc::now().to_rfc3339(),
                        "expires_at": (Utc::now() + chrono::Duration::hours(4)).to_rfc3339(),
                        "title": "Full reset"
                    }]
                }))
                .set_delay(Duration::from_millis(30)),
        )
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(/*status*/ 500))
        .expect(/*requests*/ 0)
        .mount(&server)
        .await;
    let fixture = Fixture::new().await?;
    let (session, mut turn, events) = fixture
        .session(format!("{}/backend-api", server.uri()))
        .await;
    let mode = fixture
        .execution
        .mode_for_turn(&fixture.config, &fixture.config.model_provider)
        .await?;
    let input = user_input();
    let cancellation = CancellationToken::new();
    let initial_history = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    let guard = spawn_turn_reminder(
        Arc::clone(&session),
        Arc::clone(&turn),
        Arc::clone(&fixture.execution),
        &mode,
        &input,
        &cancellation,
    )
    .expect("background pass");
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.coordinator.running.load(Ordering::Acquire) || Arc::strong_count(&turn) > 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert!(events.is_empty());
    assert!(
        !fixture
            .home
            .path()
            .join(".credit-expiry-reminders.json")
            .exists()
    );
    {
        let turn = Arc::get_mut(&mut turn).expect("finished worker released turn");
        turn.extension_data = Arc::new(codex_extension_api::ExtensionData::new("next-turn"));
        turn.sub_id = "next-turn".into();
    }
    let _guard = spawn_turn_reminder(
        Arc::clone(&session),
        Arc::clone(&turn),
        Arc::clone(&fixture.execution),
        &mode,
        &input,
        &cancellation,
    )
    .expect("cached next turn");
    let event = tokio::time::timeout(Duration::from_secs(2), events.recv()).await??;
    assert_eq!(event.id, "next-turn");
    assert!(matches!(event.msg, EventMsg::Warning(_)));
    assert!(
        spawn_turn_reminder(
            Arc::clone(&session),
            Arc::clone(&turn),
            Arc::clone(&fixture.execution),
            &mode,
            &input,
            &cancellation
        )
        .is_none()
    );
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>(),
        initial_history
    );
    Ok(())
}

#[tokio::test]
async fn credit_expiry_notice_skips_subagents_heartbeat_and_stock_provider() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let (session, mut turn, _events) = fixture
        .session("http://127.0.0.1:1/backend-api".into())
        .await;
    let mode = fixture
        .execution
        .mode_for_turn(&fixture.config, &fixture.config.model_provider)
        .await?;
    let cancellation = CancellationToken::new();
    assert!(
        spawn_turn_reminder(
            Arc::clone(&session),
            Arc::clone(&turn),
            Arc::clone(&fixture.execution),
            &ExecutionAuthMode::Stock,
            &user_input(),
            &cancellation
        )
        .is_none()
    );
    Arc::get_mut(&mut turn)
        .expect("unshared turn")
        .session_source = SessionSource::SubAgent(SubAgentSource::Review);
    assert!(
        spawn_turn_reminder(
            Arc::clone(&session),
            Arc::clone(&turn),
            Arc::clone(&fixture.execution),
            &mode,
            &user_input(),
            &cancellation
        )
        .is_none()
    );
    Arc::get_mut(&mut turn)
        .expect("unshared turn")
        .session_source = SessionSource::Exec;
    let mut heartbeat = user_input();
    if let TurnInput::UserInput { metadata, .. } = &mut heartbeat[0] {
        metadata.origin = codex_history::UserInputOrigin::Heartbeat;
    }
    assert!(
        spawn_turn_reminder(
            session,
            turn,
            Arc::clone(&fixture.execution),
            &mode,
            &heartbeat,
            &cancellation
        )
        .is_none()
    );
    assert!(!fixture.coordinator.running.load(Ordering::Acquire));
    assert!(
        !fixture
            .home
            .path()
            .join(".credit-expiry-reminders.json")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn stopped_credit_expiry_turn_cancels_reads_without_claiming_a_notice() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let started = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&started);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/rate-limit-reset-credits"))
        .respond_with(move |_: &wiremock::Request| {
            observed.store(/*val*/ true, Ordering::Release);
            ResponseTemplate::new(/*status*/ 200)
                .set_body_json(json!({
                    "available_count": 1, "credits": [{
                        "id": "voucher-1", "reset_type": "codex_rate_limits", "status": "available",
                        "granted_at": Utc::now().to_rfc3339(),
                        "expires_at": (Utc::now() + chrono::Duration::hours(4)).to_rfc3339(),
                    }]
                }))
                .set_delay(Duration::from_secs(2))
        })
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let fixture = Fixture::new().await?;
    let (session, turn, events) = fixture
        .session(format!("{}/backend-api", server.uri()))
        .await;
    let mode = fixture
        .execution
        .mode_for_turn(&fixture.config, &fixture.config.model_provider)
        .await?;
    let cancellation = CancellationToken::new();
    let _guard = spawn_turn_reminder(
        session,
        turn,
        Arc::clone(&fixture.execution),
        &mode,
        &user_input(),
        &cancellation,
    )
    .expect("pending read");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(200), async {
        while fixture.coordinator.running.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert!(events.is_empty());
    assert!(
        !fixture
            .home
            .path()
            .join(".credit-expiry-reminders.json")
            .exists()
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.method.as_str() == "GET")
    );
    Ok(())
}

#[tokio::test]
async fn credit_expiry_extension_retains_budget_without_a_far_future_warning() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    let cancellation = CancellationToken::new();
    assert!(
        fixture
            .warning(std::slice::from_ref(&fixture.snapshot), &cancellation)
            .is_some()
    );
    let mut extended = fixture.snapshot.clone();
    let expires_at = Utc::now().timestamp() + 60 * 24 * 3_600;
    extended.credits[0].expires_at = expires_at;
    assert!(fixture.warning(&[extended], &cancellation).is_none());
    let state: serde_json::Value = serde_json::from_slice(&std::fs::read(
        fixture.home.path().join(".credit-expiry-reminders.json"),
    )?)?;
    assert_eq!(
        state["records"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["expires_at"],
        expires_at
    );
    assert!(
        fixture
            .warning(std::slice::from_ref(&fixture.snapshot), &cancellation)
            .is_none()
    );
    Ok(())
}
