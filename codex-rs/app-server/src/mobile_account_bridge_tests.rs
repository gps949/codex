use super::*;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
use codex_app_server_protocol::RateLimitSnapshot;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;

fn popup_pool() -> AccountPoolReadResponse {
    serde_json::from_value(json!({
        "enabled": true, "activeProfileId": "current-profile", "activeGeneration": 4,
        "accounts": [
            {"profileId": "login-profile", "label": "Login", "priority": 30,
             "isActive": false, "availability": {"type": "authenticationUnavailable", "reason": "expired"},
             "rateLimits": {}, "email": "login@example.com", "planType": "pro"},
            {"profileId": "backup-profile", "label": "Backup", "priority": 10,
             "isActive": false, "availability": {"type": "available"},
             "rateLimits": {}, "email": "backup@example.com", "planType": "plus"},
            {"profileId": "disabled-profile", "label": "Off", "priority": 40,
             "isActive": false, "availability": {"type": "disabled"},
             "rateLimits": {}, "email": "off@example.com", "planType": "plus"},
            {"profileId": "travel-profile", "label": "Travel", "priority": 20,
             "isActive": false, "availability": {"type": "exhausted", "resetsAt": 1900000000},
             "rateLimits": {}, "email": "travel@example.com", "planType": "plus"},
            {"profileId": "current-profile", "label": "Work", "priority": 50,
             "isActive": true, "availability": {"type": "available"},
             "rateLimits": {}, "email": "work@example.com", "planType": "pro"}
        ]
    }))
    .unwrap()
}

fn popup_account_response(pool: &AccountPoolReadResponse) -> GetAccountResponse {
    GetAccountResponse {
        account: Some(Account::Chatgpt {
            email: Some("work@example.com".into()),
            plan_type: codex_protocol::account::PlanType::Pro,
        }),
        requires_openai_auth: true,
        account_pool: Some(pool.clone()),
        workspace_routing: None,
    }
}

#[test]
fn native_status_account_field_previews_current_and_standby_accounts() {
    let pool = popup_pool();
    let mut response = popup_account_response(&pool);
    let mut expected = response.clone();
    let preview = "Pool · 2/5 ready | Work · Current · Ready | Backup · Ready | +3 more · /account";
    expected.account = Some(Account::Chatgpt {
        email: Some(preview.into()),
        plan_type: codex_protocol::account::PlanType::Pro,
    });
    overlay_get_account_for_remote_client(&mut response, &pool);
    assert_eq!(response, expected);
    let Some(Account::Chatgpt { email, .. }) = response.account else {
        panic!("ChatGPT account expected");
    };
    insta::assert_snapshot!(email.unwrap(), @"Pool · 2/5 ready | Work · Current · Ready | Backup · Ready | +3 more · /account");
}

#[test]
fn native_status_account_preview_reports_each_availability() {
    let mut pool = popup_pool();
    pool.accounts.retain(|account| account.is_active);
    let mut previews = Vec::new();
    for (availability, expected) in [
        (
            codex_app_server_protocol::AccountPoolAvailability::Available,
            "Pool · 1/1 ready | Work · Current · Ready | /account",
        ),
        (
            codex_app_server_protocol::AccountPoolAvailability::Exhausted { resets_at: None },
            "Pool · 0/1 ready | Work · Current · Cooling down | /account",
        ),
        (
            codex_app_server_protocol::AccountPoolAvailability::AuthenticationUnavailable {
                reason: "expired".into(),
            },
            "Pool · 0/1 ready | Work · Current · Login required | /account",
        ),
        (
            codex_app_server_protocol::AccountPoolAvailability::Disabled,
            "Pool · 0/1 ready | Work · Current · Disabled | /account",
        ),
    ] {
        pool.accounts[0].availability = availability;
        let mut response = popup_account_response(&pool);
        overlay_get_account_for_remote_client(&mut response, &pool);
        assert_eq!(
            response.account,
            Some(Account::Chatgpt {
                email: Some(expected.into()),
                plan_type: codex_protocol::account::PlanType::Pro,
            })
        );
        let Some(Account::Chatgpt { email, .. }) = response.account else {
            panic!("ChatGPT account expected");
        };
        previews.push(email.unwrap());
    }
    insta::assert_snapshot!(previews.join("\n"), @"
    Pool · 1/1 ready | Work · Current · Ready | /account
    Pool · 0/1 ready | Work · Current · Cooling down | /account
    Pool · 0/1 ready | Work · Current · Login required | /account
    Pool · 0/1 ready | Work · Current · Disabled | /account
    ");
}

#[test]
fn native_status_account_preview_is_bounded_and_sanitizes_names() {
    let mut pool = popup_pool();
    pool.accounts[4].label = Some("\n\u{202e}`Work` [Team]\t".into());
    let mut account = pool.accounts[1].clone();
    account.label = Some("Reserve\n[account]\u{2066}".repeat(100));
    for priority in 100..200 {
        account.profile_id = format!("hidden-profile-{priority}");
        account.priority = priority;
        pool.accounts.push(account.clone());
    }
    let mut response = popup_account_response(&pool);
    overlay_get_account_for_remote_client(&mut response, &pool);
    let Some(Account::Chatgpt { email, .. }) = response.account else {
        panic!("ChatGPT account expected");
    };
    let preview = email.unwrap();
    assert!(preview.chars().count() <= 160);
    insta::assert_snapshot!(preview, @"Pool · 102/105 ready | Work Team · Current · Ready | Backup · Ready | +103 more · /account");
    for account in &mut pool.accounts {
        account.label = Some("Long account name ".repeat(100));
        account.availability =
            codex_app_server_protocol::AccountPoolAvailability::AuthenticationUnavailable {
                reason: "expired".into(),
            };
    }
    let mut response = popup_account_response(&pool);
    overlay_get_account_for_remote_client(&mut response, &pool);
    let Some(Account::Chatgpt { email, .. }) = response.account else {
        panic!("ChatGPT account expected");
    };
    let preview = email.unwrap();
    assert!(preview.chars().count() <= 160);
    assert_eq!(preview.matches("Login required").count(), 2);
    assert!(preview.ends_with("+103 more · /account"));
}

#[test]
fn native_status_account_preview_handles_missing_current_and_empty_pool() {
    let mut pool = popup_pool();
    pool.active_profile_id = None;
    for account in &mut pool.accounts {
        account.is_active = false;
    }
    let mut response = popup_account_response(&pool);
    overlay_get_account_for_remote_client(&mut response, &pool);
    let Some(Account::Chatgpt { email, .. }) = response.account else {
        panic!("ChatGPT account expected");
    };
    insta::assert_snapshot!(email.unwrap(), @"Pool · 2/5 ready | No current account | Backup · Ready | Travel · Cooling down | +3 more · /account");
    pool.accounts.clear();
    let mut response = popup_account_response(&pool);
    overlay_get_account_for_remote_client(&mut response, &pool);
    let Some(Account::Chatgpt { email, .. }) = response.account else {
        panic!("ChatGPT account expected");
    };
    insta::assert_snapshot!(email.unwrap(), @"Pool · 0/0 ready | Account pool is empty | /account");
}

#[test]
fn native_status_account_preview_keeps_unconfigured_and_other_auth_payloads() {
    let mut pool = popup_pool();
    pool.enabled = false;
    let mut response = popup_account_response(&pool);
    let expected = response.clone();
    overlay_get_account_for_remote_client(&mut response, &pool);
    assert_eq!(response, expected);
    pool.enabled = true;
    response.account = Some(Account::ApiKey {});
    let expected = response.clone();
    overlay_get_account_for_remote_client(&mut response, &pool);
    assert_eq!(response, expected);
}

#[test]
fn native_status_quota_overlay_uses_bucket_identity_and_preserves_payload() {
    let pool = popup_pool();
    let snapshot: RateLimitSnapshot = serde_json::from_value(json!({
        "limitId": "codex", "limitName": "Codex", "normalModelSlug": "gpt-5.4",
        "primary": {"usedPercent": 37, "windowDurationMins": 300, "resetsAt": 1900000000},
        "secondary": {"usedPercent": 11, "windowDurationMins": 10080, "resetsAt": 1900100000},
        "credits": {"hasCredits": true, "unlimited": false, "balance": "12.00"},
        "planType": "pro", "spendControlReached": false
    }))
    .unwrap();
    let mut other = snapshot.clone();
    other.limit_id = None;
    other.limit_name = Some("Other model".into());
    let mut response = GetAccountRateLimitsResponse {
        ordinary_usage_allowed: Some(false),
        rate_limits: snapshot.clone(),
        rate_limits_by_limit_id: Some(HashMap::from([
            ("codex".into(), snapshot),
            ("other-model".into(), other),
        ])),
        rate_limit_reset_credits: None,
        account_id: Some("active-account".into()),
        rate_limit_upsell: Some(json!({"message": "Backend message"})),
    };
    let mut expected = response.clone();
    expected.rate_limits.limit_name = Some("Work · Current quota".into());
    expected
        .rate_limits_by_limit_id
        .as_mut()
        .unwrap()
        .get_mut("codex")
        .unwrap()
        .limit_name = Some("Work · Current quota".into());
    overlay_get_account_rate_limits_for_remote_client(&mut response, &pool);
    assert_eq!(response, expected);
}

#[test]
fn native_status_quota_title_uses_profile_id_when_active_flags_are_stale() {
    let mut pool = popup_pool();
    pool.active_profile_id = Some("backup-profile".into());
    let snapshot: RateLimitSnapshot = serde_json::from_value(json!({
        "limitId": "codex", "limitName": "Codex",
        "primary": {"usedPercent": 37, "windowDurationMins": 300, "resetsAt": 1900000000}
    }))
    .unwrap();
    let mut response = GetAccountRateLimitsResponse {
        ordinary_usage_allowed: None,
        rate_limits: snapshot,
        rate_limits_by_limit_id: None,
        rate_limit_reset_credits: None,
        account_id: None,
        rate_limit_upsell: None,
    };
    let mut expected = response.clone();
    expected.rate_limits.limit_name = Some("Backup · Current quota".into());
    overlay_get_account_rate_limits_for_remote_client(&mut response, &pool);
    assert_eq!(response, expected);
}

#[tokio::test]
async fn mobile_account_refresh_keeps_caption_and_desktop_payload() {
    let (tx, mut rx) = mpsc::channel(2);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    outgoing
        .remote_clients
        .register(ConnectionId(1), "codex_chatgpt_ios_remote".to_string())
        .await;
    *outgoing.remote_clients.caption.lock().await = Some("Work · 2/3 ready".to_string());
    let snapshot: RateLimitSnapshot = serde_json::from_value(serde_json::json!({
        "limitId": "codex", "limitName": "Codex",
        "primary": {"usedPercent": 40, "windowDurationMins": 300, "resetsAt": 1900000000}
    }))
    .unwrap();
    let notification =
        ServerNotification::AccountRateLimitsUpdated(AccountRateLimitsUpdatedNotification {
            rate_limits: snapshot.clone(),
        });
    outgoing
        .send_server_notification_to_connections(
            &[ConnectionId(1), ConnectionId(2)],
            notification.clone(),
        )
        .await;
    for (id, expected) in [(ConnectionId(2), snapshot.clone())] {
        let OutgoingEnvelope::ToConnection {
            connection_id,
            message: OutgoingMessage::AppServerNotification(envelope),
            ..
        } = rx.recv().await.unwrap()
        else {
            panic!("targeted notification expected");
        };
        assert_eq!(connection_id, id);
        assert_eq!(
            serde_json::to_value(envelope.notification).unwrap(),
            serde_json::to_value(ServerNotification::AccountRateLimitsUpdated(
                AccountRateLimitsUpdatedNotification {
                    rate_limits: expected
                }
            ))
            .unwrap()
        );
    }
    assert!(
        rx.try_recv().is_err(),
        "untagged usage must not be relabeled as the selected account"
    );
    outgoing.remote_clients.unregister(ConnectionId(1)).await;
    outgoing
        .send_server_notification_to_connections(&[ConnectionId(1)], notification.clone())
        .await;
    let OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    } = rx.recv().await.unwrap()
    else {
        panic!("targeted notification expected");
    };
    assert_eq!(
        serde_json::to_value(envelope.notification).unwrap(),
        serde_json::to_value(notification).unwrap()
    );
}

#[tokio::test]
async fn mobile_account_reply_stays_on_requesting_connection() {
    use codex_app_server_protocol::TurnStartParams;
    let (tx, mut rx) = mpsc::channel(16);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let thread_id = ThreadId::new();
    let request = ConnectionRequestId {
        connection_id: ConnectionId(7),
        request_id: codex_app_server_protocol::RequestId::Integer(1),
    };
    let params: TurnStartParams = serde_json::from_value(serde_json::json!({
        "threadId": thread_id.to_string(), "input": [{"type":"text", "text":"/status", "textElements":[]}]
    })).unwrap();
    let reply =
        complete_mobile_slash_turn(&outgoing, &request, thread_id, &params, "Accounts".into())
            .await;
    assert_eq!(reply.turn.status, TurnStatus::Completed);
    let mut methods = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        let OutgoingEnvelope::ToConnection {
            connection_id,
            message: OutgoingMessage::AppServerNotification(envelope),
            ..
        } = envelope
        else {
            panic!("connection-local notification expected")
        };
        assert_eq!(connection_id, ConnectionId(7));
        methods.push(serde_json::to_value(envelope.notification).unwrap()["method"].clone());
    }
    assert_eq!(
        methods,
        serde_json::json!([
            "turn/started",
            "item/started",
            "item/completed",
            "item/started",
            "item/completed",
            "turn/completed"
        ])
        .as_array()
        .unwrap()
        .clone()
    );
}
