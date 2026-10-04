//! Exercises hot primary-login handover against a real localhost relay websocket.

use crate::primary_login_remote::PrimaryRemoteObserver;
use anyhow::Context;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::extract::WebSocketUpgrade;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use base64::Engine as _;
use codex_app_server_protocol::RemoteControlConnectionStatus;
use codex_app_server_transport::RemoteControlHandle;
use codex_app_server_transport::RemoteControlPolicy;
use codex_app_server_transport::RemoteControlStartConfig;
use codex_app_server_transport::RemoteControlStartupMode;
use codex_app_server_transport::TransportEvent;
use codex_app_server_transport::start_remote_control;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::ConfigBuilder;
use codex_http_client::DestinationPolicy;
use codex_http_client::NetworkPolicyController;
use codex_login::AccountProfile;
use codex_login::AccountProfileStore;
use codex_login::AuthConfig;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::ExternalAuthFuture;
use codex_login::PrimaryLoginPolicyLoader;
use codex_login::PrimaryLoginRuntime;
use codex_login::PrimaryLoginStore;
use codex_login::PrimaryLoginTransitionObserver;
use codex_login::save_auth;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug, Eq, PartialEq)]
enum RelayEvent {
    Enrolled {
        workspace: String,
        authorization: String,
    },
    Connected {
        server_id: String,
        authorization: String,
    },
    Closed {
        server_id: String,
    },
}

#[derive(Clone)]
struct RelayState(mpsc::UnboundedSender<RelayEvent>);

async fn enroll(State(state): State<RelayState>, headers: HeaderMap) -> Json<Value> {
    let workspace = headers["chatgpt-account-id"].to_str().unwrap().to_string();
    let authorization = headers["authorization"].to_str().unwrap().to_string();
    let _ = state.0.send(RelayEvent::Enrolled {
        workspace: workspace.clone(),
        authorization,
    });
    Json(json!({
        "server_id": format!("server-{workspace}"),
        "environment_id": format!("environment-{workspace}"),
        "remote_control_token": format!("relay-{workspace}"),
        "expires_at": "2999-01-01T00:00:00Z"
    }))
}

async fn websocket(
    State(state): State<RelayState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let server_id = headers["x-codex-server-id"].to_str().unwrap().to_string();
    let authorization = headers["authorization"].to_str().unwrap().to_string();
    upgrade.on_upgrade(move |mut socket| async move {
        let _ = state.0.send(RelayEvent::Connected {
            server_id: server_id.clone(),
            authorization,
        });
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, axum::extract::ws::Message::Close(_)) {
                break;
            }
        }
        let _ = state.0.send(RelayEvent::Closed { server_id });
    })
}

struct PolicyGate {
    controller: NetworkPolicyController,
    block_b: AtomicBool,
    entered: mpsc::UnboundedSender<()>,
    release: Semaphore,
}

impl PrimaryLoginPolicyLoader for PolicyGate {
    fn prepare(&self, source: Arc<AuthManager>) -> ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            if source
                .auth_cached()
                .and_then(|auth| auth.get_chatgpt_user_id())
                .as_deref()
                == Some("seat-b")
                && self.block_b.swap(false, Ordering::AcqRel)
            {
                let _ = self.entered.send(());
                self.release.acquire().await.unwrap().forget();
            }
            self.controller.publish(
                self.controller.policy().revision(),
                DestinationPolicy::Unrestricted,
            );
            Ok(())
        })
    }
}

struct Harness {
    home: TempDir,
    config: AuthConfig,
    runtime: Arc<PrimaryLoginRuntime>,
    remote: RemoteControlHandle,
    profile_b: AccountProfile,
    profile_c: AccountProfile,
    events: mpsc::UnboundedReceiver<RelayEvent>,
    deferred_events: VecDeque<RelayEvent>,
    closed_servers: Vec<String>,
    policy: Arc<PolicyGate>,
    policy_entered: mpsc::UnboundedReceiver<()>,
    _loader: Arc<dyn PrimaryLoginPolicyLoader>,
    _observer: Arc<dyn PrimaryLoginTransitionObserver>,
    _transport_events: mpsc::Receiver<TransportEvent>,
    shutdown: CancellationToken,
    remote_task: Option<JoinHandle<()>>,
    relay_task: JoinHandle<()>,
}

fn write_auth(home: &Path, user: &str, workspace: &str) -> anyhow::Result<()> {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({
        "email": format!("{user}@example.com"),
        "https://api.openai.com/auth": { "chatgpt_user_id": user, "chatgpt_account_id": workspace, "chatgpt_plan_type": "pro" }
    }).to_string());
    let auth: AuthDotJson = serde_json::from_value(json!({
        "auth_mode": "chatgpt",
        "tokens": { "id_token": format!("e30.{claims}.sig"), "access_token": format!("access-{user}"), "refresh_token": format!("refresh-{user}"), "account_id": workspace },
        "last_refresh": "2099-01-01T00:00:00Z"
    }))?;
    save_auth(
        home,
        &auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    Ok(())
}

impl Harness {
    async fn start(startup: RemoteControlStartupMode) -> anyhow::Result<Self> {
        let home = TempDir::new()?;
        write_auth(home.path(), "seat-a", "workspace-a")?;
        let profiles = AccountProfileStore::new(home.path().to_path_buf());
        let profile_a = profiles.allocate_profile(Some("seat-a".into()), /*priority*/ 10)?;
        write_auth(&profile_a.credential_home, "seat-a", "workspace-a")?;
        let profile_a = profiles.complete_profile(&profile_a.id)?;
        let mut ready_profiles = Vec::new();
        for (user, workspace) in [("seat-b", "workspace-b"), ("seat-c", "workspace-c")] {
            let profile = profiles.allocate_profile(Some(user.into()), /*priority*/ 10)?;
            write_auth(&profile.credential_home, user, workspace)?;
            ready_profiles.push(profiles.complete_profile(&profile.id)?);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let destination = format!("http://{}/backend-api/", listener.local_addr()?);
        let (event_tx, events) = mpsc::unbounded_channel();
        let router = Router::new()
            .route(
                "/backend-api/wham/remote/control/server/enroll",
                post(enroll),
            )
            .route("/backend-api/wham/remote/control/server", get(websocket))
            .with_state(RelayState(event_tx));
        let relay_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .cli_overrides(vec![(
                "chatgpt_base_url".into(),
                toml::Value::String(destination.clone()),
            )])
            .build()
            .await?
            .auth_config();
        config.auth_credentials_store_mode = AuthCredentialsStoreMode::File;
        let (entered, policy_entered) = mpsc::unbounded_channel();
        let policy = Arc::new(PolicyGate {
            controller: Default::default(),
            block_b: AtomicBool::new(false),
            entered,
            release: Semaphore::new(0),
        });
        config.auth_route_config = AuthRouteConfig::from_http_client_factory(
            codex_login::test_support::transport_default_auth_route_config()
                .http_client_factory()
                .clone()
                .with_network_policy(policy.controller.policy()),
        );
        PrimaryLoginStore::new(home.path().to_path_buf())
            .select_profile(&config, &profile_a.id)
            .await?;
        let runtime = PrimaryLoginRuntime::start(config.clone()).await?;
        let loader: Arc<dyn PrimaryLoginPolicyLoader> = policy.clone();
        runtime.set_policy_loader(Arc::downgrade(&loader));
        let state = StateRuntime::init(
            SqliteConfig::new_for_testing(AbsolutePathBuf::from_absolute_path(home.path())?),
            "test-provider".into(),
        )
        .await?;
        let shutdown = CancellationToken::new();
        let (transport_event_tx, transport_events) = mpsc::channel(32);
        let (remote_task, remote) = start_remote_control(
            RemoteControlStartConfig {
                remote_control_url: destination.clone(),
                installation_id: "11111111-1111-4111-8111-111111111111".into(),
                policy: RemoteControlPolicy::Allowed,
            },
            Some(state),
            runtime.auth_manager(),
            transport_event_tx,
            shutdown.clone(),
            /*app_server_client_name_rx*/ None,
            startup,
        )
        .await?;
        let observer =
            PrimaryRemoteObserver::new(config.clone(), &runtime, remote.clone(), destination);
        let observer: Arc<dyn PrimaryLoginTransitionObserver> = observer;
        runtime.set_transition_observer(Arc::downgrade(&observer));
        let mut ready_profiles = ready_profiles.into_iter();
        Ok(Self {
            home,
            config,
            runtime,
            remote,
            profile_b: ready_profiles.next().unwrap(),
            profile_c: ready_profiles.next().unwrap(),
            events,
            deferred_events: VecDeque::new(),
            closed_servers: Vec::new(),
            policy,
            policy_entered,
            _loader: loader,
            _observer: observer,
            _transport_events: transport_events,
            shutdown,
            remote_task: Some(remote_task),
            relay_task,
        })
    }

    fn store(&self) -> PrimaryLoginStore {
        PrimaryLoginStore::new(self.home.path().to_path_buf())
    }

    async fn event(&mut self) -> anyhow::Result<RelayEvent> {
        if let Some(event) = self.deferred_events.pop_front() {
            return Ok(event);
        }
        timeout(DEADLINE, async {
            loop {
                match self
                    .events
                    .recv()
                    .await
                    .context("relay event stream ended")?
                {
                    RelayEvent::Closed { server_id } => self.closed_servers.push(server_id),
                    event => return Ok(event),
                }
            }
        })
        .await?
    }

    async fn closed(&mut self, workspace: &str) -> anyhow::Result<()> {
        let expected = format!("server-{workspace}");
        timeout(DEADLINE, async {
            while !self.closed_servers.contains(&expected) {
                match self
                    .events
                    .recv()
                    .await
                    .context("relay event stream ended")?
                {
                    RelayEvent::Closed { server_id } => self.closed_servers.push(server_id),
                    event => self.deferred_events.push_back(event),
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await?
    }

    async fn connected(&mut self, user: &str, workspace: &str) -> anyhow::Result<()> {
        assert_eq!(
            self.event().await?,
            RelayEvent::Enrolled {
                workspace: workspace.into(),
                authorization: format!("Bearer access-{user}")
            }
        );
        assert_eq!(
            self.event().await?,
            RelayEvent::Connected {
                server_id: format!("server-{workspace}"),
                authorization: format!("Bearer relay-{workspace}")
            }
        );
        let mut changes = self.remote.status_receiver();
        let environment = format!("environment-{workspace}");
        timeout(
            DEADLINE,
            changes.wait_for(|status| {
                status.status == RemoteControlConnectionStatus::Connected
                    && status.environment_id.as_deref() == Some(environment.as_str())
            }),
        )
        .await??;
        Ok(())
    }

    fn assert_owner(&self, user: &str, workspace: &str) {
        let auth = self.runtime.auth_manager().auth_cached().unwrap();
        assert_eq!(
            (auth.get_chatgpt_user_id(), auth.get_account_id()),
            (Some(user.into()), Some(workspace.into()))
        );
    }

    async fn block_b(&mut self) -> anyhow::Result<()> {
        self.policy.block_b.store(true, Ordering::Release);
        self.store()
            .select_profile(&self.config, &self.profile_b.id)
            .await?;
        self.runtime.sync().await?;
        timeout(DEADLINE, self.policy_entered.recv())
            .await?
            .context("policy worker stopped")?;
        self.closed("workspace-a").await?;
        Ok(())
    }

    async fn no_relay_revival(&mut self) -> anyhow::Result<()> {
        // A bounded observation covers the background continuation task and checks actual I/O.
        assert!(
            self.deferred_events.is_empty(),
            "unexpected deferred relay request"
        );
        assert!(
            timeout(Duration::from_millis(500), self.events.recv())
                .await
                .is_err(),
            "disabled Remote unexpectedly contacted the relay"
        );
        assert_eq!(
            self.remote.status().status,
            RemoteControlConnectionStatus::Disabled
        );
        Ok(())
    }

    async fn finish(mut self) -> anyhow::Result<()> {
        self.shutdown.cancel();
        timeout(DEADLINE, self.remote_task.take().unwrap()).await??;
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.policy.release.add_permits(1);
        self.relay_task.abort();
    }
}

fn host_managed_identity_requires_separate_fixture() -> bool {
    if codex_login::is_workload_identity_selected() {
        tracing::debug!(
            "skipping synthetic primary-source test under host-managed workload identity"
        );
        return true;
    }
    false
}

#[tokio::test]
async fn explicit_primary_selection_reconnects_new_owner_without_restart() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    let manager = harness.runtime.auth_manager();
    let root_credentials = std::fs::read(harness.home.path().join("auth.json"))?;
    harness
        .store()
        .select_profile(&harness.config, &harness.profile_b.id)
        .await?;
    harness.runtime.sync().await?;
    assert!(Arc::ptr_eq(&manager, &harness.runtime.auth_manager()));
    harness.assert_owner("seat-b", "workspace-b");
    harness.closed("workspace-a").await?;
    harness.connected("seat-b", "workspace-b").await?;
    assert_eq!(
        std::fs::read(harness.home.path().join("auth.json"))?,
        root_credentials
    );
    harness.finish().await
}

#[tokio::test]
async fn primary_selection_keeps_disabled_remote_disabled() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::DisabledEphemeral).await?;
    harness
        .store()
        .select_profile(&harness.config, &harness.profile_b.id)
        .await?;
    harness.runtime.sync().await?;
    harness.assert_owner("seat-b", "workspace-b");
    harness.no_relay_revival().await?;
    harness.finish().await
}

#[tokio::test]
async fn implicit_root_relogin_retires_relay_without_continuation() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    if std::env::var_os(codex_login::CODEX_ACCESS_TOKEN_ENV_VAR).is_some() {
        tracing::debug!("skipping root-login fixture because a host access token is configured");
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    write_auth(harness.home.path(), "replacement-user", "workspace-a")?;
    harness.store().use_root()?;
    harness.runtime.sync().await?;
    harness.assert_owner("replacement-user", "workspace-a");
    harness.closed("workspace-a").await?;
    harness.no_relay_revival().await?;
    harness.finish().await
}

#[tokio::test]
async fn newer_explicit_selection_supersedes_pending_remote_handover() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    harness.block_b().await?;
    harness
        .store()
        .select_profile(&harness.config, &harness.profile_c.id)
        .await?;
    let runtime = harness.runtime.clone();
    let sync = tokio::spawn(async move { runtime.sync().await });
    tokio::task::yield_now().await;
    harness.policy.release.add_permits(1);
    timeout(DEADLINE, sync).await???;
    harness.assert_owner("seat-c", "workspace-c");
    harness.connected("seat-c", "workspace-c").await?;
    harness.finish().await
}

#[tokio::test]
async fn explicit_remote_disable_cancels_pending_primary_continuation() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    harness.block_b().await?;
    harness.remote.disable_ephemeral().await;
    harness.policy.release.add_permits(1);
    timeout(DEADLINE, harness.runtime.auth_manager().auth()).await?;
    harness.no_relay_revival().await?;
    harness.finish().await
}

#[tokio::test]
async fn host_logout_cancels_pending_primary_continuation() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    harness.block_b().await?;
    harness.store().sign_out()?;
    let runtime = harness.runtime.clone();
    let sync = tokio::spawn(async move { runtime.sync().await });
    tokio::task::yield_now().await;
    harness.policy.release.add_permits(1);
    timeout(DEADLINE, sync).await???;
    assert!(harness.runtime.auth_manager().auth_cached().is_none());
    harness.no_relay_revival().await?;
    harness.finish().await
}

#[tokio::test]
async fn replacement_profile_owner_cancels_pending_primary_continuation() -> anyhow::Result<()> {
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    harness.block_b().await?;
    write_auth(
        &harness.profile_b.credential_home,
        "unexpected-seat",
        "workspace-b",
    )?;
    harness.policy.release.add_permits(1);
    assert!(timeout(DEADLINE, harness.runtime.sync()).await?.is_err());
    assert!(harness.runtime.auth_manager().auth_cached().is_none());
    harness.no_relay_revival().await?;
    harness.finish().await
}

#[tokio::test]
async fn request_resolution_before_watcher_preserves_explicit_remote_handover() -> anyhow::Result<()>
{
    if host_managed_identity_requires_separate_fixture() {
        return Ok(());
    }
    let mut harness = Harness::start(RemoteControlStartupMode::EnabledEphemeral).await?;
    harness.connected("seat-a", "workspace-a").await?;
    PrimaryLoginStore::new(harness.home.path().to_path_buf())
        .select_profile(&harness.config, &harness.profile_b.id)
        .await?;
    // Resolve first, before the local source watcher applies the selection. The hook must
    // capture the old enabled permission before request-time auth adopts the new identity.
    let auth = harness
        .runtime
        .auth_manager()
        .auth_with_http_client_factory()
        .await
        .unwrap()
        .0;
    assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("seat-b"));
    harness.runtime.sync().await?;
    harness.connected("seat-b", "workspace-b").await?;
    harness.finish().await?;
    Ok(())
}
