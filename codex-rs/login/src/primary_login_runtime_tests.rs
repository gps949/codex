use std::fs;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::PrimaryLoginRuntime;
use crate::AccountProfileMetadataUpdate;
use crate::AccountProfileStore;
use crate::PrimaryLoginPolicyLoader;
use crate::PrimaryLoginStore;
use crate::primary_login::tests::config;
use crate::primary_login::tests::credentials;
use crate::primary_login::tests::ready_profile;
use crate::primary_login::tests::write_auth;

fn owner(auth: Option<crate::CodexAuth>) -> Option<(String, String)> {
    let auth = auth?;
    Some((auth.get_chatgpt_user_id()?, auth.get_account_id()?))
}

#[tokio::test]
async fn stable_primary_manager_follows_explicit_source_without_changing_pool_selection() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let profile_a = ready_profile(home.path(), "seat-a", "team-workspace");
    let profile_b = ready_profile(home.path(), "seat-b", "team-workspace");
    let runtime_path = home.path().join("account-runtime-state.json");
    fs::write(&runtime_path, "unchanged inference state").unwrap();
    let root_before = fs::read(home.path().join("auth.json")).unwrap();
    let cfg = config(home.path().to_path_buf());
    let runtime = PrimaryLoginRuntime::start(cfg.clone()).await.unwrap();
    let facade = runtime.auth_manager();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    assert_eq!(
        owner(facade.auth().await),
        Some(("root-user".into(), "root-workspace".into()))
    );
    for profile in [&profile_a, &profile_b] {
        store.select_profile(&cfg, &profile.id).await.unwrap();
        runtime.sync().await.unwrap();
        assert!(Arc::ptr_eq(&facade, &runtime.auth_manager()));
        assert_eq!(
            facade.auth().await.unwrap().get_chatgpt_user_id().unwrap(),
            if profile.id == profile_a.id {
                "seat-a"
            } else {
                "seat-b"
            }
        );
    }
    store.sign_out().unwrap();
    runtime.sync().await.unwrap();
    assert_eq!(owner(facade.auth().await), None);
    assert_eq!(
        fs::read(home.path().join("auth.json")).unwrap(),
        root_before
    );
    assert_eq!(
        fs::read_to_string(&runtime_path).unwrap(),
        "unchanged inference state"
    );
}

#[tokio::test]
async fn same_owner_credential_changes_preserve_remote_owner_generation() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "seat-a", "workspace");
    let cfg = config(home.path().to_path_buf());
    PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&cfg, &profile.id)
        .await
        .unwrap();
    let runtime = PrimaryLoginRuntime::start(cfg).await.unwrap();
    let facade = runtime.auth_manager();
    let changes = facade.auth_change_state_receiver();
    let before = *changes.borrow();
    write_auth(
        &profile.credential_home,
        &credentials("seat-a", "workspace", "renewed"),
    );
    runtime.sync().await.unwrap();
    let after = *changes.borrow();
    assert_eq!(after.owner_generation, before.owner_generation);
    assert!(after.generation > before.generation);
    assert_eq!(
        facade.auth_cached().unwrap().get_token().unwrap(),
        "access-renewed"
    );
}

#[tokio::test]
async fn profile_relogin_to_another_seat_in_the_same_workspace_clears_primary_auth() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let profile = ready_profile(home.path(), "seat-a", "workspace");
    let cfg = config(home.path().to_path_buf());
    PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&cfg, &profile.id)
        .await
        .unwrap();
    let runtime = PrimaryLoginRuntime::start(cfg).await.unwrap();
    let facade = runtime.auth_manager();
    let before = facade
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    write_auth(
        &profile.credential_home,
        &credentials("seat-b", "workspace", "changed-seat"),
    );
    assert!(runtime.sync().await.is_err());
    assert_eq!(owner(facade.auth().await), None);
    assert!(
        facade
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            > before
    );
}

#[tokio::test]
async fn disabling_inference_does_not_sign_primary_out() {
    let home = TempDir::new().unwrap();
    let profile = ready_profile(home.path(), "seat-a", "workspace");
    let cfg = config(home.path().to_path_buf());
    PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&cfg, &profile.id)
        .await
        .unwrap();
    let runtime = PrimaryLoginRuntime::start(cfg).await.unwrap();
    AccountProfileStore::new(home.path().to_path_buf())
        .update_profile_metadata(
            &profile.id,
            AccountProfileMetadataUpdate {
                disabled: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    runtime.sync().await.unwrap();
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("seat-a".into(), "workspace".into()))
    );
}

#[tokio::test]
async fn missing_selected_profile_does_not_fall_back_to_root_or_another_pool_profile() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let profile = ready_profile(home.path(), "seat-a", "workspace");
    let _other = ready_profile(home.path(), "seat-b", "workspace");
    let cfg = config(home.path().to_path_buf());
    PrimaryLoginStore::new(home.path().to_path_buf())
        .select_profile(&cfg, &profile.id)
        .await
        .unwrap();
    let runtime = PrimaryLoginRuntime::start(cfg).await.unwrap();
    // Simulate a manifest written by an older installation without the new deletion guard.
    let profiles = AccountProfileStore::new(home.path().to_path_buf());
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(profiles.manifest_path()).unwrap()).unwrap();
    manifest["profiles"]
        .as_array_mut()
        .unwrap()
        .retain(|entry| entry["id"].as_str() != Some(profile.id.as_str()));
    fs::write(
        profiles.manifest_path(),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(runtime.sync().await.is_err());
    assert_eq!(owner(runtime.auth_manager().auth().await), None);
}

#[tokio::test]
async fn corrupt_primary_metadata_revokes_existing_auth_and_stays_signed_out_after_restart() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let cfg = config(home.path().to_path_buf());
    let runtime = PrimaryLoginRuntime::start(cfg.clone()).await.unwrap();
    assert!(runtime.auth_manager().auth().await.is_some());
    fs::write(
        PrimaryLoginStore::new(home.path().to_path_buf()).path(),
        "{",
    )
    .unwrap();
    assert!(runtime.sync().await.is_err());
    assert_eq!(owner(runtime.auth_manager().auth().await), None);
    let restarted = PrimaryLoginRuntime::start(cfg).await.unwrap();
    assert_eq!(owner(restarted.auth_manager().auth().await), None);
}

#[tokio::test]
async fn external_root_login_changes_publish_the_actual_new_primary_owner() {
    let home = TempDir::new().unwrap();
    write_auth(home.path(), &credentials("root-a", "workspace-a", "before"));
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    let facade = runtime.auth_manager();
    let before = facade
        .auth_change_state_receiver()
        .borrow()
        .owner_generation;
    write_auth(home.path(), &credentials("root-b", "workspace-b", "after"));
    runtime.sync().await.unwrap();
    assert_eq!(
        owner(facade.auth().await),
        Some(("root-b".into(), "workspace-b".into()))
    );
    assert!(
        facade
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            > before
    );
}

#[tokio::test]
async fn pool_added_after_startup_does_not_take_over_primary_auth() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    let _profile = ready_profile(home.path(), "seat-a", "workspace");
    runtime.sync().await.unwrap();
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("root-user".into(), "root-workspace".into()))
    );
}

#[tokio::test]
async fn signed_out_primary_does_not_parse_unrelated_root_personal_access_token() {
    let home = TempDir::new().unwrap();
    let mut auth = credentials("root-user", "root-workspace", "root");
    auth.auth_mode = Some(codex_protocol::auth::AuthMode::PersonalAccessToken);
    auth.personal_access_token = Some("synthetic-pat-that-must-not-be-hydrated".into());
    write_auth(home.path(), &auth);
    PrimaryLoginStore::new(home.path().to_path_buf())
        .sign_out()
        .unwrap();
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    runtime.sync().await.unwrap();
    assert_eq!(owner(runtime.auth_manager().auth().await), None);
}

#[tokio::test]
async fn idle_root_pat_observation_defers_hydration_and_retires_previous_oauth_owner() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let cfg = config(home.path().to_path_buf());
    let runtime = PrimaryLoginRuntime::start(cfg.clone()).await.unwrap();
    assert!(runtime.auth_manager().auth_cached().is_some());
    let mut pat = credentials("unused-user", "unused-workspace", "unused");
    pat.auth_mode = Some(codex_protocol::auth::AuthMode::PersonalAccessToken);
    pat.personal_access_token = Some("synthetic-pat-that-must-not-be-hydrated".into());
    write_auth(home.path(), &pat);
    assert_eq!(
        runtime.sync().await.unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(owner(runtime.auth_manager().auth_cached()), None);
    let restarted = PrimaryLoginRuntime::start(cfg).await.unwrap();
    assert_eq!(
        restarted.sync().await.unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(owner(restarted.auth_manager().auth_cached()), None);
}

#[tokio::test]
async fn explicit_root_revision_reloads_process_local_tokens_with_unchanged_persistent_auth() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let root_before = fs::read(home.path().join("auth.json")).unwrap();
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    let external = credentials("external-user", "external-workspace", "external");
    crate::auth::login_with_chatgpt_auth_tokens(
        home.path(),
        &external.tokens.unwrap().id_token.raw_jwt,
        "external-workspace",
        Some("pro"),
    )
    .unwrap();
    store.use_root().unwrap();
    runtime.sync().await.unwrap();
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("external-user".into(), "external-workspace".into()))
    );
    assert_eq!(
        fs::read(home.path().join("auth.json")).unwrap(),
        root_before
    );
}

#[derive(Default)]
struct RecordingPolicy {
    owners: Mutex<Vec<(String, String)>>,
    deny: AtomicBool,
}

impl PrimaryLoginPolicyLoader for RecordingPolicy {
    fn prepare(&self, manager: Arc<crate::AuthManager>) -> crate::ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            self.owners
                .lock()
                .unwrap()
                .push(owner(manager.auth_cached()).unwrap());
            if self.deny.load(Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "synthetic host policy refusal",
                ));
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn requests_prepare_the_selected_owner_policy_and_observations_do_not() {
    let home = TempDir::new().unwrap();
    let a = ready_profile(home.path(), "seat-a", "workspace");
    let b = ready_profile(home.path(), "seat-b", "workspace");
    let cfg = config(home.path().to_path_buf());
    let store = PrimaryLoginStore::new(home.path().to_path_buf());
    store.select_profile(&cfg, &a.id).await.unwrap();
    let runtime = PrimaryLoginRuntime::start(cfg.clone()).await.unwrap();
    let policy = Arc::new(RecordingPolicy::default());
    let loader: Arc<dyn PrimaryLoginPolicyLoader> = policy.clone();
    runtime.set_policy_loader(Arc::downgrade(&loader));
    runtime.sync().await.unwrap();
    assert_eq!(
        *policy.owners.lock().unwrap(),
        Vec::<(String, String)>::new()
    );
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("seat-a".into(), "workspace".into()))
    );
    store.select_profile(&cfg, &b.id).await.unwrap();
    runtime.sync().await.unwrap();
    policy.deny.store(true, Ordering::Release);
    let changes = runtime.auth_manager().auth_change_state_receiver();
    let before = changes.borrow().owner_generation;
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("seat-b".into(), "workspace".into()))
    );
    assert_eq!(changes.borrow().owner_generation, before);
    policy.deny.store(false, Ordering::Release);
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("seat-b".into(), "workspace".into()))
    );
    assert_eq!(changes.borrow().owner_generation, before);
    assert_eq!(
        *policy.owners.lock().unwrap(),
        vec![
            ("seat-a".into(), "workspace".into()),
            ("seat-b".into(), "workspace".into()),
            ("seat-b".into(), "workspace".into())
        ]
    );
}

struct HeldPolicy {
    entered: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

impl PrimaryLoginPolicyLoader for HeldPolicy {
    fn prepare(&self, _manager: Arc<crate::AuthManager>) -> crate::ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            self.entered.notify_one();
            self.released.notified().await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn sign_out_clears_cached_owner_before_waiting_for_in_flight_policy_load() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("root-user", "root-workspace", "root"),
    );
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    let facade = runtime.auth_manager();
    let policy = Arc::new(HeldPolicy {
        entered: tokio::sync::Notify::new(),
        released: tokio::sync::Notify::new(),
    });
    let loader: Arc<dyn PrimaryLoginPolicyLoader> = policy.clone();
    runtime.set_policy_loader(Arc::downgrade(&loader));
    let request = tokio::spawn({
        let facade = Arc::clone(&facade);
        async move { facade.auth().await }
    });
    tokio::time::timeout(Duration::from_secs(/*secs*/ 2), policy.entered.notified())
        .await
        .unwrap();
    PrimaryLoginStore::new(home.path().to_path_buf())
        .sign_out()
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(/*millis*/ 50), runtime.sync()).await;
    assert_eq!(owner(facade.auth_cached()), None);
    policy.released.notify_one();
    assert_eq!(
        owner(
            tokio::time::timeout(Duration::from_secs(/*secs*/ 2), request)
                .await
                .unwrap()
                .unwrap()
        ),
        None
    );
}

#[tokio::test]
async fn external_root_tokens_take_precedence_over_corrupt_unrelated_persistent_auth() {
    let home = TempDir::new().unwrap();
    fs::write(home.path().join("auth.json"), "{").unwrap();
    let external = credentials("external-user", "external-workspace", "external");
    crate::auth::login_with_chatgpt_auth_tokens(
        home.path(),
        &external.tokens.unwrap().id_token.raw_jwt,
        "external-workspace",
        Some("pro"),
    )
    .unwrap();
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    runtime.sync().await.unwrap();
    assert_eq!(
        owner(runtime.auth_manager().auth().await),
        Some(("external-user".into(), "external-workspace".into()))
    );
    assert_eq!(
        fs::read_to_string(home.path().join("auth.json")).unwrap(),
        "{"
    );
}

#[tokio::test]
async fn external_root_observations_keep_the_same_live_owner_generation() {
    let home = TempDir::new().unwrap();
    let external = credentials("external-user", "external-workspace", "external");
    crate::auth::login_with_chatgpt_auth_tokens(
        home.path(),
        &external.tokens.unwrap().id_token.raw_jwt,
        "external-workspace",
        Some("pro"),
    )
    .unwrap();
    let runtime = PrimaryLoginRuntime::start(config(home.path().to_path_buf()))
        .await
        .unwrap();
    let manager = runtime.auth_manager();
    assert_eq!(
        owner(manager.auth().await),
        Some(("external-user".into(), "external-workspace".into()))
    );
    let changes = manager.auth_change_state_receiver();
    let before = changes.borrow().owner_generation;
    for _ in 0..3 {
        runtime.sync().await.unwrap();
    }
    assert_eq!(changes.borrow().owner_generation, before);
    assert_eq!(
        owner(manager.auth_cached()),
        Some(("external-user".into(), "external-workspace".into()))
    );
}

struct TemporaryPolicy {
    controller: codex_http_client::NetworkPolicyController,
    unavailable: AtomicBool,
}

impl PrimaryLoginPolicyLoader for TemporaryPolicy {
    fn prepare(&self, _manager: Arc<crate::AuthManager>) -> crate::ExternalAuthFuture<'_, ()> {
        Box::pin(async move {
            let revision = self.controller.policy().revision();
            if self.unavailable.load(Ordering::Acquire) {
                self.controller.unavailable(revision);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "synthetic policy timeout",
                ));
            }
            self.controller
                .publish(revision, codex_http_client::DestinationPolicy::Unrestricted);
            Ok(())
        })
    }
}

#[tokio::test]
async fn temporary_policy_unavailability_preserves_login_and_retries_with_network_closed() {
    let home = TempDir::new().unwrap();
    write_auth(
        home.path(),
        &credentials("remote-user", "remote-workspace", "root"),
    );
    let policy = Arc::new(TemporaryPolicy {
        controller: Default::default(),
        unavailable: AtomicBool::new(true),
    });
    let mut cfg = config(home.path().to_path_buf());
    let factory = cfg
        .auth_route_config
        .http_client_factory()
        .clone()
        .with_network_policy(policy.controller.policy());
    cfg.auth_route_config = crate::AuthRouteConfig::from_http_client_factory(factory);
    let runtime = PrimaryLoginRuntime::start(cfg).await.unwrap();
    let loader: Arc<dyn PrimaryLoginPolicyLoader> = policy.clone();
    runtime.set_policy_loader(Arc::downgrade(&loader));
    let manager = runtime.auth_manager();
    let changes = manager.auth_change_state_receiver();
    let before = changes.borrow().owner_generation;
    assert_eq!(
        owner(manager.auth().await),
        Some(("remote-user".into(), "remote-workspace".into()))
    );
    assert_eq!(changes.borrow().owner_generation, before);
    let endpoint = "https://chatgpt.com/backend-api/codex/remote-control/server/enroll"
        .parse()
        .unwrap();
    assert_eq!(
        manager
            .http_client_factory()
            .network_policy()
            .acquire(&endpoint)
            .unwrap_err(),
        codex_http_client::NetworkPolicyDenied::Unavailable
    );
    policy.unavailable.store(false, Ordering::Release);
    assert_eq!(
        owner(manager.auth().await),
        Some(("remote-user".into(), "remote-workspace".into()))
    );
    assert_eq!(changes.borrow().owner_generation, before);
    assert!(
        manager
            .http_client_factory()
            .network_policy()
            .acquire(&endpoint)
            .is_ok()
    );
}
