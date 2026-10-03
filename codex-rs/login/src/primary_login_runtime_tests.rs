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
