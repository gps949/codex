//! Stable-protocol bridges for ChatGPT mobile remote clients that do not yet
//! parse experimental `accountPool/*` RPCs or `accountPool` JSON fields.
//!
//! These helpers repurpose interfaces mobile already renders:
//! - `account/read` (`account.email` overlay)
//! - `account/workspaceMessages/read` (headline banners)
//! - `warning` notifications (ephemeral status toasts)
//! - `turn/start` for `/account` and `/status` (connection-local replies without model history)
//! - `account/rateLimits/read` (`rateLimits.limitName` overlay for the status panel)

use std::collections::HashMap;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::Utc;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountPoolAccount;
use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetWorkspaceMessagesResponse;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_app_server_protocol::WarningNotification;
use codex_app_server_protocol::WorkspaceMessage;
use codex_app_server_protocol::WorkspaceMessageType;
use codex_login::format_exhausted_reset_unix;
use codex_protocol::ThreadId;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;

pub(crate) const CHATGPT_REMOTE_CLIENT_NAMES: &[&str] =
    &["codex_chatgpt_android_remote", "codex_chatgpt_ios_remote"];

const LOCAL_WORKSPACE_MESSAGE_ID: &str = "codex-local-account-pool";

pub(crate) fn is_chatgpt_remote_client(client_name: Option<&str>) -> bool {
    client_name.is_some_and(|name| CHATGPT_REMOTE_CLIENT_NAMES.contains(&name))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MobileSlashCommand {
    Account,
    Status,
}

pub(crate) fn mobile_slash_command(
    input: &[V2UserInput],
    client_name: Option<&str>,
) -> Option<MobileSlashCommand> {
    if !is_chatgpt_remote_client(client_name) {
        return None;
    }
    let text = single_text_input(input)?;
    if matches_slash_command(text, "account") {
        Some(MobileSlashCommand::Account)
    } else if matches_slash_command(text, "status") {
        Some(MobileSlashCommand::Status)
    } else {
        None
    }
}

fn single_text_input(input: &[V2UserInput]) -> Option<&str> {
    match input {
        [V2UserInput::Text { text, .. }] => Some(text.as_str()),
        _ => None,
    }
}

fn matches_slash_command(text: &str, command: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return false;
    }
    let rest = trimmed.trim_start_matches('/').trim_start();
    rest.split_whitespace()
        .next()
        .is_some_and(|token| token.eq_ignore_ascii_case(command))
}

fn now_unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) async fn complete_mobile_slash_turn(
    outgoing: &OutgoingMessageSender,
    request_id: &ConnectionRequestId,
    thread_id: ThreadId,
    params: &TurnStartParams,
    pool: &AccountPoolReadResponse,
    command: MobileSlashCommand,
) -> TurnStartResponse {
    let connection_ids = [request_id.connection_id];
    let turn_id = Uuid::new_v4().to_string();
    let user_item_id = Uuid::new_v4().to_string();
    let agent_item_id = Uuid::new_v4().to_string();
    let thread_id_string = thread_id.to_string();
    let started_at_ms = now_unix_timestamp_ms();
    let default_command = match command {
        MobileSlashCommand::Account => "/account",
        MobileSlashCommand::Status => "/status",
    };
    let user_text = single_text_input(&params.input)
        .map(str::trim)
        .unwrap_or(default_command)
        .to_string();
    let agent_text = match command {
        MobileSlashCommand::Account => mobile_account_slash_reply(pool),
        MobileSlashCommand::Status => mobile_status_slash_reply(pool),
    };

    outgoing.record_request_turn_id(request_id, &turn_id).await;

    let in_progress_turn = Turn {
        id: turn_id.clone(),
        items: vec![],
        items_view: TurnItemsView::NotLoaded,
        error: None,
        status: TurnStatus::InProgress,
        started_at: Some(started_at_ms / 1000),
        completed_at: None,
        duration_ms: None,
    };
    outgoing
        .send_server_notification_to_connections(
            &connection_ids,
            ServerNotification::TurnStarted(TurnStartedNotification {
                thread_id: thread_id_string.clone(),
                turn: in_progress_turn,
            }),
        )
        .await;

    let user_item = ThreadItem::UserMessage {
        id: user_item_id.clone(),
        client_id: params.client_user_message_id.clone(),
        content: vec![V2UserInput::Text {
            text: user_text.clone(),
            text_elements: Vec::new(),
        }],
    };
    emit_item_lifecycle(
        outgoing,
        &connection_ids,
        &thread_id_string,
        &turn_id,
        user_item.clone(),
        started_at_ms,
    )
    .await;

    let agent_item = ThreadItem::AgentMessage {
        id: agent_item_id.clone(),
        text: agent_text.clone(),
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    };
    emit_item_lifecycle(
        outgoing,
        &connection_ids,
        &thread_id_string,
        &turn_id,
        agent_item.clone(),
        started_at_ms,
    )
    .await;

    let completed_at_ms = now_unix_timestamp_ms();
    let completed_turn = Turn {
        id: turn_id.clone(),
        items: vec![agent_item.clone()],
        items_view: TurnItemsView::Summary,
        error: None,
        status: TurnStatus::Completed,
        started_at: Some(started_at_ms / 1000),
        completed_at: Some(completed_at_ms / 1000),
        duration_ms: Some((completed_at_ms - started_at_ms).max(0)),
    };
    outgoing
        .send_server_notification_to_connections(
            &connection_ids,
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: thread_id_string,
                turn: completed_turn.clone(),
            }),
        )
        .await;

    TurnStartResponse {
        turn: completed_turn,
    }
}

fn mobile_account_slash_reply(pool: &AccountPoolReadResponse) -> String {
    let mut lines = vec![format_account_pool_summary(pool)];
    if pool.enabled {
        lines.push(String::new());
        lines.push(host_account_switch_hint());
    }
    lines.join("\n")
}

fn mobile_status_slash_reply(pool: &AccountPoolReadResponse) -> String {
    format!(
        "{}\nUsage bars show the executing account's limits. Ready counts describe scheduler availability, not verified quota.",
        crate::mobile_account_status::account_caption(pool)
    )
}

fn host_account_switch_hint() -> String {
    r#"Switch profiles on the host with `codex account use "<label-or-id>"`."#.to_string()
}

pub(crate) fn overlay_get_account_rate_limits_for_remote_client(
    response: &mut GetAccountRateLimitsResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    let Some(overlay) = crate::mobile_account_status::pool_caption(pool) else {
        return;
    };
    crate::mobile_account_status::overlay_snapshot(&mut response.rate_limits, &overlay);
    if let Some(rate_limits_by_limit_id) = response.rate_limits_by_limit_id.as_mut() {
        for snapshot in rate_limits_by_limit_id.values_mut() {
            crate::mobile_account_status::overlay_snapshot(snapshot, &overlay);
        }
    }
}

async fn emit_item_lifecycle(
    outgoing: &OutgoingMessageSender,
    connection_ids: &[ConnectionId],
    thread_id: &str,
    turn_id: &str,
    item: ThreadItem,
    started_at_ms: i64,
) {
    outgoing
        .send_server_notification_to_connections(
            connection_ids,
            ServerNotification::ItemStarted(ItemStartedNotification {
                item: item.clone(),
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                started_at_ms,
            }),
        )
        .await;
    outgoing
        .send_server_notification_to_connections(
            connection_ids,
            ServerNotification::ItemCompleted(ItemCompletedNotification {
                item,
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                completed_at_ms: now_unix_timestamp_ms(),
            }),
        )
        .await;
}

#[derive(Default)]
pub(crate) struct RemoteClientRegistry {
    clients: Mutex<HashMap<ConnectionId, String>>,
    pub(crate) caption: Mutex<Option<String>>,
}

impl RemoteClientRegistry {
    pub(crate) async fn register(&self, connection_id: ConnectionId, client_name: String) {
        if is_chatgpt_remote_client(Some(client_name.as_str())) {
            self.clients.lock().await.insert(connection_id, client_name);
        }
    }

    pub(crate) async fn unregister(&self, connection_id: ConnectionId) {
        self.clients.lock().await.remove(&connection_id);
    }

    pub(crate) async fn decorate_notification(
        &self,
        connection_id: ConnectionId,
        notification: &mut ServerNotification,
    ) {
        if let ServerNotification::AccountRateLimitsUpdated(update) = notification
            && self.clients.lock().await.contains_key(&connection_id)
            && let Some(caption) = self.caption.lock().await.as_deref()
        {
            crate::mobile_account_status::overlay_snapshot(&mut update.rate_limits, caption);
        }
    }
}

pub(crate) fn format_account_pool_summary(pool: &AccountPoolReadResponse) -> String {
    if !pool.enabled {
        return "Multi-account pool is not configured. Add profiles with `codex account add` on the host.".to_string();
    }

    let mut lines = vec![format!(
        "Codex account pool · active: {}",
        crate::mobile_account_status::compact_label(
            pool.active_profile_id.as_deref().unwrap_or("automatic"),
            48
        )
    )];
    for account in pool.accounts.iter().take(8) {
        lines.push(format!(
            "· {} (priority {}) — {}",
            account_display_label(account),
            account.priority,
            availability_label(account)
        ));
    }
    if pool.accounts.len() > 8 {
        lines.push(format!(
            "{} more accounts. Run `codex account list` on the host.",
            pool.accounts.len() - 8
        ));
    }
    lines.join("\n")
}

pub(crate) fn overlay_get_account_for_remote_client(
    response: &mut GetAccountResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    let summary = crate::mobile_account_status::account_caption(pool);
    if let Some(Account::Chatgpt { email, .. }) = response.account.as_mut() {
        *email = Some(summary);
    }
}

pub(crate) fn inject_workspace_messages_for_remote_client(
    response: &mut GetWorkspaceMessagesResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    response.messages.insert(
        0,
        WorkspaceMessage {
            message_id: LOCAL_WORKSPACE_MESSAGE_ID.to_string(),
            message_type: WorkspaceMessageType::Headline,
            message_body: crate::mobile_account_status::account_caption(pool),
            created_at: Some(Utc::now().timestamp()),
            archived_at: None,
        },
    );
}

pub(crate) async fn push_account_pool_warning(
    outgoing: &OutgoingMessageSender,
    connection_ids: &[ConnectionId],
    pool: &AccountPoolReadResponse,
) {
    if connection_ids.is_empty() || !pool.enabled {
        return;
    }
    let notification = ServerNotification::Warning(WarningNotification {
        thread_id: None,
        message: crate::mobile_account_status::account_caption(pool),
    });
    outgoing
        .send_server_notification_to_connections(connection_ids, notification)
        .await;
}

fn account_display_label(account: &AccountPoolAccount) -> String {
    let id = crate::mobile_account_status::compact_label(&account.profile_id, 48);
    match account.label.as_deref().or(account.email.as_deref()) {
        Some(label) => format!(
            "{id} ({})",
            crate::mobile_account_status::compact_label(label, 32)
        ),
        None => id,
    }
}

fn availability_label(account: &AccountPoolAccount) -> String {
    match &account.availability {
        AccountPoolAvailability::Available => "available".to_string(),
        AccountPoolAvailability::Exhausted { resets_at } => match resets_at {
            Some(until) => format!("cooldown until {}", format_exhausted_reset_unix(*until)),
            None => "cooling down".to_string(),
        },
        AccountPoolAvailability::AuthenticationUnavailable { .. } => {
            "login required on host".to_string()
        }
        AccountPoolAvailability::Disabled => "disabled".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::AccountPoolRateLimits;
    use pretty_assertions::assert_eq;

    #[test]
    fn remote_client_detection_matches_ios_and_android_names() {
        assert!(is_chatgpt_remote_client(Some("codex_chatgpt_ios_remote")));
        assert!(is_chatgpt_remote_client(Some(
            "codex_chatgpt_android_remote"
        )));
        assert!(!is_chatgpt_remote_client(Some("codex-tui")));
    }

    #[test]
    fn mobile_status_slash_command_matches_plain_input() {
        let input = vec![V2UserInput::Text {
            text: "/status".to_string(),
            text_elements: Vec::new(),
        }];
        assert_eq!(
            mobile_slash_command(&input, Some("codex_chatgpt_ios_remote")),
            Some(MobileSlashCommand::Status)
        );
    }

    #[test]
    fn rate_limits_overlay_replaces_limit_name_with_pool_summary() {
        let pool = AccountPoolReadResponse {
            enabled: true,
            active_profile_id: Some("primary".to_string()),
            active_generation: Some(1),
            accounts: vec![],
        };
        let mut response = GetAccountRateLimitsResponse {
            rate_limits: codex_app_server_protocol::RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("Codex".to_string()),
                primary: None,
                secondary: None,
                credits: None,
                individual_limit: None,
                spend_control_reached: None,
                plan_type: None,
                rate_limit_reached_type: None,
            },
            rate_limits_by_limit_id: None,
            rate_limit_reset_credits: None,
            account_id: None,
            rate_limit_upsell: None,
        };
        overlay_get_account_rate_limits_for_remote_client(&mut response, &pool);
        assert!(
            response
                .rate_limits
                .limit_name
                .as_deref()
                .is_some_and(|name| name == "Codex · 0/0 ready")
        );
    }

    #[test]
    fn status_overlay_keeps_progress_windows_and_other_buckets() {
        let pool = AccountPoolReadResponse {
            enabled: true,
            active_profile_id: Some("primary".to_string()),
            active_generation: Some(1),
            accounts: vec![AccountPoolAccount {
                profile_id: "primary".to_string(),
                label: Some("Work".to_string()),
                priority: 0,
                is_active: true,
                availability: AccountPoolAvailability::Available,
                plan_type: None,
                email: None,
                rate_limits: AccountPoolRateLimits::default(),
            }],
        };
        let snapshot: codex_app_server_protocol::RateLimitSnapshot = serde_json::from_value(serde_json::json!({
            "limitId": "codex", "limitName": "Codex",
            "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1900000000},
            "secondary": {"usedPercent": 10, "windowDurationMins": 10080, "resetsAt": 1900100000}
        })).unwrap();
        let mut other = snapshot.clone();
        other.limit_id = Some("other".to_string());
        other.limit_name = Some("Other model".to_string());
        let mut response = GetAccountRateLimitsResponse {
            rate_limits: snapshot.clone(),
            rate_limits_by_limit_id: Some(HashMap::from([
                ("codex".to_string(), snapshot.clone()),
                ("other".to_string(), other.clone()),
            ])),
            rate_limit_reset_credits: None,
            account_id: None,
            rate_limit_upsell: None,
        };
        overlay_get_account_rate_limits_for_remote_client(&mut response, &pool);
        let mut expected = snapshot;
        expected.limit_name = Some("Codex · 1/1 ready".to_string());
        assert_eq!(response.rate_limits, expected);
        assert_eq!(
            response.rate_limits_by_limit_id,
            Some(HashMap::from([
                ("codex".to_string(), expected),
                ("other".to_string(), other)
            ]))
        );
        insta::assert_snapshot!(response.rate_limits.limit_name.unwrap(), @"Codex · 1/1 ready");
    }

    #[test]
    fn mobile_account_slash_command_matches_plain_and_spaced_input() {
        let input = vec![V2UserInput::Text {
            text: "  /account  ".to_string(),
            text_elements: Vec::new(),
        }];
        assert_eq!(
            mobile_slash_command(&input, Some("codex_chatgpt_ios_remote")),
            Some(MobileSlashCommand::Account)
        );
        assert_eq!(mobile_slash_command(&input, Some("codex-tui")), None);
    }

    #[test]
    fn mobile_account_slash_command_rejects_non_text_or_multi_item_input() {
        let input = vec![
            V2UserInput::Text {
                text: "/account".to_string(),
                text_elements: Vec::new(),
            },
            V2UserInput::Text {
                text: "extra".to_string(),
                text_elements: Vec::new(),
            },
        ];
        assert_eq!(
            mobile_slash_command(&input, Some("codex_chatgpt_ios_remote")),
            None
        );
    }

    #[test]
    fn mobile_account_slash_reply_includes_host_switch_hint_when_pool_enabled() {
        let pool = AccountPoolReadResponse {
            enabled: true,
            active_profile_id: Some("primary".to_string()),
            active_generation: Some(1),
            accounts: vec![],
        };
        let reply = mobile_account_slash_reply(&pool);
        assert!(reply.contains("codex account use"));
    }

    #[test]
    fn workspace_message_injection_prepends_pool_headline() {
        let pool = AccountPoolReadResponse {
            enabled: true,
            active_profile_id: Some("primary".to_string()),
            active_generation: Some(1),
            accounts: vec![AccountPoolAccount {
                profile_id: "primary".to_string(),
                label: Some("Team".to_string()),
                priority: 0,
                is_active: true,
                availability: AccountPoolAvailability::Available,
                plan_type: None,
                email: None,
                rate_limits: AccountPoolRateLimits::default(),
            }],
        };
        let mut response = GetWorkspaceMessagesResponse {
            feature_enabled: true,
            messages: vec![WorkspaceMessage {
                message_id: "backend-1".to_string(),
                message_type: WorkspaceMessageType::Announcement,
                message_body: "Existing headline".to_string(),
                created_at: None,
                archived_at: None,
            }],
        };
        inject_workspace_messages_for_remote_client(&mut response, &pool);
        assert_eq!(response.messages.len(), 2);
        assert_eq!(response.messages[0].message_id, LOCAL_WORKSPACE_MESSAGE_ID);
        assert!(response.messages[0].message_body.contains("Team"));
    }
}

#[cfg(test)]
#[path = "mobile_account_bridge_tests.rs"]
mod ux_tests;
