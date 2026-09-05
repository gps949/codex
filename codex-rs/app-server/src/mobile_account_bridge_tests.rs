use super::*;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
use codex_app_server_protocol::RateLimitSnapshot;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;

#[tokio::test]
async fn mobile_account_refresh_keeps_caption_and_desktop_payload() {
    let (tx, mut rx) = mpsc::channel(2);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    outgoing
        .remote_clients
        .register(ConnectionId(1), "codex_chatgpt_ios_remote".to_string())
        .await;
    *outgoing.remote_clients.caption.lock().await = Some("Codex · 2/3 ready".to_string());
    let snapshot: RateLimitSnapshot = serde_json::from_value(serde_json::json!({
        "limitId": "codex", "limitName": "Codex",
        "primary": {"usedPercent": 40, "windowDurationMins": 300, "resetsAt": 1900000000}
    }))
    .unwrap();
    let mut mobile = snapshot.clone();
    mobile.limit_name = Some("Codex · 2/3 ready".to_string());
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
    for (id, expected) in [
        (ConnectionId(1), mobile),
        (ConnectionId(2), snapshot.clone()),
    ] {
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
