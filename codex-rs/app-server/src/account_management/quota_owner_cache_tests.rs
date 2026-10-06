use super::super::report::RefreshPermit;
use super::*;
use pretty_assertions::assert_eq;

fn write_owner(fixture: &Fixture, user: &str, workspace: &str) -> anyhow::Result<()> {
    let profile = fixture.manager.profile(fixture.id.as_str())?;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({
            "https://api.openai.com/auth":{"chatgpt_user_id":user,"chatgpt_account_id":workspace}
        })
        .to_string(),
    );
    std::fs::write(
        profile.profile.credential_home.join("auth.json"),
        serde_json::to_vec(&json!({
            "tokens":{"id_token":format!("e30.{claims}.sig"),"access_token":format!("synthetic-{user}"),
                "refresh_token":"synthetic-refresh","account_id":workspace},
            "last_refresh":"2099-01-01T00:00:00Z"
        }))?,
    )?;
    Ok(())
}

fn usage(fixture: &Fixture, user: &str, workspace: &str, count: Option<u64>) -> serde_json::Value {
    let mut response = json!({"account_id":workspace,"user_id":user,"plan_type":"pro",
        "rate_limit":{"allowed":false,"limit_reached":true,
            "primary_window":{"used_percent":100,"limit_window_seconds":18000,"reset_at":fixture.reset.timestamp(),"reset_after_seconds":7200},
            "secondary_window":{"used_percent":100,"limit_window_seconds":604800,"reset_at":fixture.reset.timestamp(),"reset_after_seconds":7200}},
        "spend_control":{"reached":false}});
    if let Some(count) = count {
        response["rate_limit_reset_credits"] = json!({"available_count":count});
    }
    response
}

#[tokio::test]
async fn relogin_owner_hides_the_previous_owners_receipt_and_credit_count() -> anyhow::Result<()> {
    for (user, workspace) in [
        ("other-user", "fixture-account"),
        ("fixture-owner", "other-workspace"),
    ] {
        let fixture = Fixture::new().await?;
        let (_, original) = fixture
            .refresh(usage(&fixture, "fixture-owner", "fixture-account", Some(3)))
            .await?;
        assert_eq!(original.reset_credit_count, Some(3));
        assert!(original.refresh.is_some());
        let original_owner = fixture.manager.profile_identity(fixture.id.as_str())?;
        let runtime = std::fs::read(fixture.home.path().join("account-runtime-state.json"))?;
        write_owner(&fixture, user, workspace)?;
        assert_ne!(
            fixture.manager.profile_identity(fixture.id.as_str())?,
            original_owner
        );
        let current = fixture.manager.inventory().await?.accounts.remove(0);
        assert_eq!(
            serde_json::to_value((current.refresh, current.reset_credit_count))?,
            json!([null, null])
        );
        assert_eq!(
            std::fs::read(fixture.home.path().join("account-runtime-state.json"))?,
            runtime
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_new_owner_refreshes_while_the_old_owner_request_is_still_running() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    fixture
        .refresh(usage(&fixture, "fixture-owner", "fixture-account", Some(3)))
        .await?;
    let old_key = (
        fixture.id.to_string(),
        fixture.manager.profile_identity(fixture.id.as_str())?,
    );
    let started = Arc::new(tokio::sync::Notify::new());
    let seen = Arc::clone(&started);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer synthetic-access",
        ))
        .respond_with(move |_: &wiremock::Request| {
            seen.notify_one();
            ResponseTemplate::new(/*s*/ 503)
                .set_delay(std::time::Duration::from_millis(/*millis*/ 500))
        })
        .with_priority(/*p*/ 1)
        .expect(/*r*/ 1)
        .mount(&fixture.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer synthetic-new-owner",
        ))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(usage(
            &fixture,
            "new-owner",
            "new-workspace",
            Some(7),
        )))
        .with_priority(/*p*/ 1)
        .expect(/*r*/ 1)
        .mount(&fixture.server)
        .await;
    let request = || {
        fixture.manager.execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
    };
    let new_owner = async {
        tokio::time::timeout(
            std::time::Duration::from_secs(/*secs*/ 2),
            started.notified(),
        )
        .await?;
        write_owner(&fixture, "new-owner", "new-workspace")?;
        let hidden = fixture.manager.inventory().await?.accounts.remove(0);
        assert_eq!(
            serde_json::to_value((hidden.refresh, hidden.reset_credit_count))?,
            json!([null, null])
        );
        let result = request().await?;
        assert!(!result.message.contains("1 already checking"));
        let current = fixture.manager.inventory().await?.accounts.remove(0);
        assert_eq!(current.reset_credit_count, Some(7));
        Ok::<_, anyhow::Error>(serde_json::to_value(current.refresh)?)
    };
    let (old_result, new_receipt) = tokio::join!(request(), new_owner);
    assert!(old_result?.message.contains("1 failed"));
    let after_old = fixture.manager.inventory().await?.accounts.remove(0);
    assert_eq!(serde_json::to_value(after_old.refresh)?, new_receipt?);
    assert_eq!(after_old.reset_credit_count, Some(7));
    assert!(
        !fixture
            .manager
            .refreshes
            .lock()
            .unwrap()
            .get(&old_key)
            .unwrap()
            .in_progress
    );
    Ok(())
}

#[tokio::test]
async fn old_owner_cancellation_only_updates_its_original_tuple() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let old_key = (
        fixture.id.to_string(),
        fixture.manager.profile_identity(fixture.id.as_str())?,
    );
    let receipt = |count| super::super::RefreshStatus {
        in_progress: true,
        attempted_at: 123,
        succeeded: false,
        message: "Checking fresh quota…".into(),
        reset_credit_count: Some(count),
    };
    fixture
        .manager
        .refreshes
        .lock()
        .unwrap()
        .insert(old_key.clone(), receipt(/*count*/ 3));
    let permit = RefreshPermit {
        statuses: Arc::clone(&fixture.manager.refreshes),
        key: old_key.clone(),
        completed: false,
    };
    write_owner(&fixture, "new-owner", "new-workspace")?;
    let new_key = (
        fixture.id.to_string(),
        fixture.manager.profile_identity(fixture.id.as_str())?,
    );
    fixture
        .manager
        .refreshes
        .lock()
        .unwrap()
        .insert(new_key.clone(), receipt(/*count*/ 7));
    let before = serde_json::to_value(fixture.manager.refreshes.lock().unwrap().get(&new_key))?;
    drop(permit);
    let statuses = fixture.manager.refreshes.lock().unwrap();
    assert_eq!(serde_json::to_value(statuses.get(&new_key))?, before);
    assert!(!statuses.get(&old_key).unwrap().in_progress);
    assert_eq!(statuses.get(&old_key).unwrap().reset_credit_count, Some(3));
    Ok(())
}

#[tokio::test]
async fn partial_and_failed_reads_retain_only_the_matching_owners_count() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    fixture
        .refresh(usage(&fixture, "fixture-owner", "fixture-account", Some(3)))
        .await?;
    write_owner(&fixture, "new-owner", "new-workspace")?;
    let mut partial = usage(&fixture, "new-owner", "new-workspace", /*count*/ None);
    partial["rate_limit"]["allowed"] = json!(true);
    partial["rate_limit"]["limit_reached"] = json!(false);
    partial["rate_limit"]
        .as_object_mut()
        .unwrap()
        .remove("secondary_window");
    let fallback = Arc::new(std::sync::Mutex::new(partial));
    let current_response = Arc::clone(&fallback);
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer synthetic-new-owner",
        ))
        .respond_with(move |_: &wiremock::Request| {
            let response = current_response.lock().unwrap();
            if response.is_null() {
                ResponseTemplate::new(/*s*/ 503)
            } else {
                ResponseTemplate::new(/*s*/ 200).set_body_json(response.clone())
            }
        })
        .with_priority(/*p*/ 1)
        .mount(&fixture.server)
        .await;
    let refresh = || {
        fixture.manager.execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
    };
    assert!(refresh().await?.message.contains("1 incomplete"));
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        None
    );
    *fallback.lock().unwrap() = usage(&fixture, "new-owner", "new-workspace", Some(7));
    refresh().await?;
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        Some(7)
    );
    let mut partial = usage(&fixture, "new-owner", "new-workspace", /*count*/ None);
    partial["rate_limit"]["allowed"] = json!(true);
    partial["rate_limit"]["limit_reached"] = json!(false);
    partial["rate_limit"]
        .as_object_mut()
        .unwrap()
        .remove("secondary_window");
    *fallback.lock().unwrap() = partial;
    refresh().await?;
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        Some(7)
    );
    *fallback.lock().unwrap() = serde_json::Value::Null;
    assert!(refresh().await?.message.contains("1 failed"));
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        Some(7)
    );
    let key = (
        fixture.id.to_string(),
        fixture.manager.profile_identity(fixture.id.as_str())?,
    );
    let permit = RefreshPermit {
        statuses: Arc::clone(&fixture.manager.refreshes),
        key: key.clone(),
        completed: false,
    };
    fixture
        .manager
        .refreshes
        .lock()
        .unwrap()
        .get_mut(&key)
        .unwrap()
        .in_progress = true;
    drop(permit);
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        Some(7)
    );
    assert_eq!(fixture.manager.refreshes.lock().unwrap().len(), 1);
    Ok(())
}

#[tokio::test]
async fn unavailable_identity_has_no_previous_owner_cache_and_old_completed_entries_are_bounded()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    fixture
        .refresh(usage(&fixture, "fixture-owner", "fixture-account", Some(3)))
        .await?;
    let credential_path = fixture
        .manager
        .profile(fixture.id.as_str())?
        .profile
        .credential_home
        .join("auth.json");
    std::fs::write(credential_path, b"{invalid synthetic credential record")?;
    let current = fixture.manager.inventory().await?.accounts.remove(0);
    assert_eq!(
        serde_json::to_value((current.refresh, current.reset_credit_count))?,
        json!([null, null])
    );
    let result = fixture
        .manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
        .await?;
    assert!(result.message.contains("1 failed"));
    assert!(result.message.contains("identity could not be verified"));
    write_owner(&fixture, "new-owner", "new-workspace")?;
    let stale = super::super::RefreshStatus {
        in_progress: false,
        attempted_at: 0,
        succeeded: false,
        message: "old-owner".into(),
        reset_credit_count: Some(3),
    };
    {
        let mut statuses = fixture.manager.refreshes.lock().unwrap();
        for index in 0..super::super::MAX_REFRESH_RECEIPTS {
            statuses.insert(
                (format!("removed-profile-{index}"), "old-owner".into()),
                stale.clone(),
            );
            statuses.insert(
                (fixture.id.to_string(), format!("old-owner-{index}")),
                stale.clone(),
            );
        }
    }
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer synthetic-new-owner",
        ))
        .respond_with(ResponseTemplate::new(/*s*/ 503))
        .with_priority(/*p*/ 1)
        .mount(&fixture.server)
        .await;
    fixture
        .manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
        .await?;
    assert_eq!(fixture.manager.refreshes.lock().unwrap().len(), 1);
    assert_eq!(
        fixture.manager.inventory().await?.accounts[0].reset_credit_count,
        None
    );
    Ok(())
}

#[tokio::test]
async fn a_full_inflight_cache_refuses_new_reservations_without_evicting_old_permits()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let running = super::super::RefreshStatus {
        in_progress: true,
        attempted_at: 1,
        succeeded: false,
        message: "Checking fresh quota…".into(),
        reset_credit_count: Some(3),
    };
    {
        let mut statuses = fixture.manager.refreshes.lock().unwrap();
        for index in 0..super::super::MAX_REFRESH_RECEIPTS {
            statuses.insert(
                (fixture.id.to_string(), format!("old-owner-{index}")),
                running.clone(),
            );
        }
    }
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(/*s*/ 503))
        .expect(/*r*/ 0)
        .mount(&fixture.server)
        .await;
    let result = fixture
        .manager
        .execute(AccountManagerOperation::Refresh {
            profile_ids: Some(vec![fixture.id.to_string()]),
        })
        .await?;
    assert!(result.message.contains("1 failed"));
    assert!(result.message.contains("Too many quota checks"));
    let statuses = fixture.manager.refreshes.lock().unwrap();
    assert_eq!(statuses.len(), super::super::MAX_REFRESH_RECEIPTS);
    assert!(statuses.values().all(|status| status.in_progress));
    Ok(())
}
