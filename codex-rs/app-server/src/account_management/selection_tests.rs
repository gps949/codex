use super::*;
use base64::Engine;
use pretty_assertions::assert_eq;

async fn fixture() -> anyhow::Result<(
    tempfile::TempDir,
    Arc<AccountManager>,
    Vec<codex_login::AccountProfileId>,
)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.toml"),
        "cli_auth_credentials_store = 'file'\n[account_pool]\nrotation_strategy = 'fill_first'\n",
    )?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let mut ids = Vec::new();
    let mut saved = Vec::new();
    let now = chrono::Utc::now();
    for (name, priority, reset_seconds) in [("first", 10, 3600), ("second", 20, 600)] {
        let profile = store.allocate_profile(Some(name.into()), priority)?;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "email": format!("{name}@example.com"), "https://api.openai.com/auth": {
                    "chatgpt_account_id": "synthetic-workspace", "chatgpt_user_id": name,
                    "chatgpt_plan_type": "business",
                }
            })
            .to_string(),
        );
        std::fs::write(
            profile.credential_home.join("auth.json"),
            serde_json::to_vec(&serde_json::json!({
                "tokens": {"id_token":format!("e30.{claims}.sig"),"access_token":format!("synthetic-{name}"),
                    "refresh_token":"synthetic-refresh","account_id":"synthetic-workspace"},
                "last_refresh":"2099-01-01T00:00:00Z"
            }))?,
        )?;
        store.complete_profile(&profile.id)?;
        saved.push(serde_json::json!({"profile_id":profile.id,"rate_limits":{
            "primary":{"used_percent":41.0,"window_minutes":300,"resets_at":now+chrono::Duration::seconds(reset_seconds)},
            "observed_at":now,"primary_observed_at":now
        }}));
        ids.push(profile.id);
    }
    std::fs::write(
        home.path().join("account-runtime-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":1,"active_profile_id":ids[0],"profiles":saved
        }))?,
    )?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    Ok((home, AccountManager::new(config), ids))
}

#[tokio::test]
async fn automatic_selection_applies_hot_strategy_and_preserves_cooldowns() -> anyhow::Result<()> {
    let (home, manager, ids) = fixture().await?;
    manager
        .execute(AccountManagerOperation::Settings {
            values: serde_json::json!({"rotation_strategy":"earliest_reset"}),
        })
        .await?;
    manager.execute(AccountManagerOperation::Automatic).await?;
    let state = AccountRuntimeStateStore::new(home.path().to_path_buf());
    assert_eq!(state.load()?.active_profile_id, Some(ids[1].clone()));
    assert_eq!(
        manager
            .current_pool_settings()
            .await?
            .effective_rotation_strategy(),
        codex_config::AccountPoolRotationStrategy::EarliestReset
    );
    let mut saved = state.load()?;
    let until = chrono::Utc::now() + chrono::Duration::hours(1);
    saved
        .profiles
        .iter_mut()
        .find(|profile| profile.profile_id == ids[1])
        .unwrap()
        .exhausted_until = Some(until);
    state.save(&saved)?;
    manager.execute(AccountManagerOperation::Automatic).await?;
    let current = state.load()?;
    assert_eq!(current.active_profile_id, Some(ids[0].clone()));
    assert_eq!(
        current
            .profiles
            .iter()
            .find(|profile| profile.profile_id == ids[1])
            .unwrap()
            .exhausted_until,
        Some(until)
    );
    Ok(())
}

#[tokio::test]
async fn automatic_selection_cancelled_context_does_not_change_selection() -> anyhow::Result<()> {
    let (home, manager, _) = fixture().await?;
    let store = AccountRuntimeStateStore::new(home.path().to_path_buf());
    let before = store.load()?;
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(
        manager
            .execute_with_context(
                AccountManagerOperation::Automatic,
                &AccountOperationContext::NativeMenu(cancelled)
            )
            .await
            .is_err()
    );
    assert_eq!(store.load()?, before);
    Ok(())
}

#[tokio::test]
async fn manager_settings_keep_session_override_above_later_user_changes() -> anyhow::Result<()> {
    let (home, _, _) = fixture().await?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .cli_overrides(vec![(
            "account_pool.rotation_strategy".into(),
            toml::Value::String("fill_first".into()),
        )])
        .build()
        .await?;
    let manager = AccountManager::new(config);
    manager
        .execute(AccountManagerOperation::Settings {
            values: serde_json::json!({"rotation_strategy":"earliest_reset"}),
        })
        .await?;
    assert_eq!(
        manager
            .current_pool_settings()
            .await?
            .effective_rotation_strategy(),
        codex_config::AccountPoolRotationStrategy::FillFirst
    );
    Ok(())
}

#[tokio::test]
async fn manual_selection_rejects_unusable_auth_without_changing_the_target() -> anyhow::Result<()>
{
    for restricted in [false, true] {
        let (home, initial_manager, ids) = fixture().await?;
        let record = initial_manager.profile(ids[1].as_str())?;
        let mut config = initial_manager.config.as_ref().clone();
        if restricted {
            config.forced_chatgpt_workspace_id = Some(vec!["another-workspace".into()]);
        } else {
            std::fs::remove_file(record.profile.credential_home.join("auth.json"))?;
        }
        let manager = AccountManager::new(config);
        let before = AccountRuntimeStateStore::new(home.path().to_path_buf()).load()?;
        assert_eq!(
            manager
                .inventory()
                .await?
                .accounts
                .iter()
                .find(|account| account.profile_id == ids[1].as_str())
                .unwrap()
                .login_state,
            "needsLogin"
        );
        for operation in [
            AccountManagerOperation::Use {
                profile_id: ids[1].to_string(),
            },
            AccountManagerOperation::Retry {
                profile_id: ids[1].to_string(),
            },
        ] {
            assert!(manager.execute(operation).await.is_err());
            assert_eq!(
                AccountRuntimeStateStore::new(home.path().to_path_buf()).load()?,
                before
            );
        }
    }
    Ok(())
}
