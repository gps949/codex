//! Explicit Remote disable must block handover capture before its async transaction commits.

use super::*;
use codex_config::ManagedAuthPolicy;
use codex_login::AuthConfig;
use codex_login::PrimaryLoginHandover;
use codex_login::PrimaryLoginStore;
use pretty_assertions::assert_eq;

const TEST_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum DisableMode {
    Ephemeral,
    Durable,
}

impl DisableMode {
    async fn run(
        self,
        handle: RemoteControlHandle,
    ) -> io::Result<RemoteControlStatusChangedNotification> {
        match self {
            Self::Ephemeral => Ok(handle.disable_ephemeral().await),
            Self::Durable => handle.disable(/*app_server_client_name*/ None).await,
        }
    }
}

struct HandoverFixture {
    _home: TempDir,
    handle: RemoteControlHandle,
    intent: PrimaryLoginHandover,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl HandoverFixture {
    async fn enabled() -> Self {
        let home = TempDir::new().expect("temporary home");
        save_auth(
            home.path(),
            &remote_control_auth_dot_json(Some("account_id")),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("save synthetic auth");
        let config = AuthConfig {
            codex_home: home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            chatgpt_base_url: Some(TEST_REMOTE_CONTROL_URL.into()),
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: ManagedAuthPolicy::default(),
            auth_route_config: codex_login::test_support::transport_default_auth_route_config(),
        };
        let intent = PrimaryLoginStore::new(home.path().to_path_buf())
            .select_root(&config)
            .await
            .expect("select synthetic root")
            .remote_handover
            .expect("complete root owner grants an intent");
        let manager = AuthManager::shared_managed_profile_from_auth_config(config).await;
        assert!(intent.matches_auth(&manager.auth_cached().expect("synthetic managed identity")));
        let shutdown = CancellationToken::new();
        let (events, _receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let (task, handle) = start_remote_control(
            RemoteControlStartConfig {
                remote_control_url: TEST_REMOTE_CONTROL_URL.into(),
                installation_id: TEST_INSTALLATION_ID.into(),
                policy: RemoteControlPolicy::Allowed,
            },
            Some(remote_control_state_runtime(&home).await),
            manager,
            events,
            shutdown.clone(),
            /*app_server_client_name_rx*/ None,
            RemoteControlStartupMode::DisabledEphemeral,
        )
        .await
        .expect("create controller");
        // Keep the controller alive: durable disable must be able to persist its preference.
        // The closed loopback endpoint can never enroll or contact a real account service.
        handle
            .enable_ephemeral()
            .expect("enable cached control state");
        assert!(handle.prepare_primary_login_handover().is_some());
        Self {
            _home: home,
            handle,
            intent,
            shutdown,
            task,
        }
    }

    async fn finish(self) {
        self.shutdown.cancel();
        timeout(TEST_DEADLINE, self.task)
            .await
            .expect("stop fixture transport")
            .expect("fixture joined");
    }

    fn manually_enable(&self) {
        self.handle
            .enable_ephemeral()
            .expect("explicit enable restores handover permission");
        assert!(self.handle.prepare_primary_login_handover().is_some());
    }
}

fn external_host_configured() -> bool {
    if codex_login::is_workload_identity_selected()
        || std::env::var_os(codex_login::CODEX_ACCESS_TOKEN_ENV_VAR).is_some()
    {
        eprintln!("skipping synthetic root handover fixture under external host authentication");
        return true;
    }
    false
}

async fn assert_capture_blocked_before_commit(mode: DisableMode) {
    let fixture = HandoverFixture::enabled().await;
    let session = fixture.handle.inner.session();
    let transition = Arc::clone(&session.desired_state_rpc_lock)
        .acquire_owned()
        .await
        .expect("hold desired-state transaction");
    let previous = fixture
        .handle
        .prepare_primary_login_handover()
        .expect("capture enabled session before command");
    let handle = fixture.handle.clone();
    let disable = tokio::spawn(async move { mode.run(handle).await });
    timeout(TEST_DEADLINE, async {
        while fixture.handle.prepare_primary_login_handover().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disable start must synchronously block later capture");
    assert!(
        session.desired_state_tx.borrow().is_enabled(),
        "fixture must hold disable before its desired-state commit"
    );
    assert!(
        !disable.is_finished(),
        "disable must still wait for its transaction"
    );
    assert!(
        !fixture
            .handle
            .continue_primary_login_handover(&previous, &fixture.intent)
            .expect("check old capability")
    );
    drop(transition);
    let status = timeout(TEST_DEADLINE, disable)
        .await
        .expect("disable completes after lock release")
        .expect("disable joined")
        .expect("disable succeeded");
    assert_eq!(status.status, RemoteControlConnectionStatus::Disabled);
    assert!(fixture.handle.prepare_primary_login_handover().is_none());
    fixture.manually_enable();
    // Explicit enable can create a new capability, but the older one stays revoked.
    assert!(
        !fixture
            .handle
            .continue_primary_login_handover(&previous, &fixture.intent)
            .expect("old capability remains revoked")
    );
    let current = fixture
        .handle
        .prepare_primary_login_handover()
        .expect("capture explicitly re-enabled session");
    assert!(
        fixture
            .handle
            .continue_primary_login_handover(&current, &fixture.intent)
            .expect("current owner can continue")
    );
    assert!(
        !fixture
            .handle
            .continue_primary_login_handover(&current, &fixture.intent)
            .expect("capability is consumed once")
    );
    fixture.finish().await;
}

async fn assert_canceled_disable_keeps_capture_blocked(mode: DisableMode) {
    let fixture = HandoverFixture::enabled().await;
    let session = fixture.handle.inner.session();
    let transition = Arc::clone(&session.desired_state_rpc_lock)
        .acquire_owned()
        .await
        .expect("hold desired-state transaction");
    let previous = fixture
        .handle
        .prepare_primary_login_handover()
        .expect("capture initial permission");
    let handle = fixture.handle.clone();
    let disable = tokio::spawn(async move { mode.run(handle).await });
    timeout(TEST_DEADLINE, async {
        while fixture.handle.prepare_primary_login_handover().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("disable start blocks capture before the pending operation is canceled");
    assert!(session.desired_state_tx.borrow().is_enabled());
    assert!(!disable.is_finished());
    disable.abort();
    assert!(
        disable
            .await
            .expect_err("pending disable must be canceled")
            .is_cancelled()
    );
    drop(transition);
    // A canceled asynchronous transaction must not undo the explicit disable intention.
    assert!(session.desired_state_tx.borrow().is_enabled());
    assert!(fixture.handle.prepare_primary_login_handover().is_none());
    assert!(
        !fixture
            .handle
            .continue_primary_login_handover(&previous, &fixture.intent)
            .expect("canceled command keeps old handover revoked")
    );
    fixture.manually_enable();
    fixture.finish().await;
}

#[tokio::test]
async fn primary_login_handover_capture_is_blocked_before_ephemeral_disable_commits() {
    if external_host_configured() {
        return;
    }
    assert_capture_blocked_before_commit(DisableMode::Ephemeral).await;
}

#[tokio::test]
async fn primary_login_handover_capture_is_blocked_before_durable_disable_commits() {
    if external_host_configured() {
        return;
    }
    assert_capture_blocked_before_commit(DisableMode::Durable).await;
}

#[tokio::test]
async fn primary_login_handover_canceled_ephemeral_disable_keeps_capture_blocked() {
    if external_host_configured() {
        return;
    }
    assert_canceled_disable_keeps_capture_blocked(DisableMode::Ephemeral).await;
}

#[tokio::test]
async fn primary_login_handover_canceled_durable_disable_keeps_capture_blocked() {
    if external_host_configured() {
        return;
    }
    assert_canceled_disable_keeps_capture_blocked(DisableMode::Durable).await;
}
