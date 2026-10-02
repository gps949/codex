//! Stable-protocol bridges for ChatGPT mobile remote clients that do not yet
//! parse experimental `accountPool/*` RPCs or `accountPool` JSON fields.
//!
//! These helpers repurpose interfaces mobile already renders:
//! - `account/read` (bounded current/standby preview in `account.email`)
//! - `account/workspaceMessages/read` (headline banners)
//! - `warning` notifications (ephemeral status toasts)
//! - `turn/start` for `/account` and `/status` (connection-local replies without model history)
//! - `account/rateLimits/read` (`rateLimits.limitName` overlay for the status panel)

use std::collections::HashMap;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use chrono::Utc;
use codex_app_server_protocol::Account;
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
    agent_text: String,
) -> TurnStartResponse {
    let connection_ids = [request_id.connection_id];
    let turn_id = Uuid::new_v4().to_string();
    let user_item_id = Uuid::new_v4().to_string();
    let agent_item_id = Uuid::new_v4().to_string();
    let thread_id_string = thread_id.to_string();
    let started_at_ms = now_unix_timestamp_ms();
    let user_text = single_text_input(&params.input)
        .map(str::trim)
        .unwrap_or("/account")
        .to_string();

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

pub(crate) fn overlay_get_account_rate_limits_for_remote_client(
    response: &mut GetAccountRateLimitsResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    let Some(overlay) = crate::mobile_account_status::quota_caption(pool) else {
        return;
    };
    crate::mobile_account_status::overlay_snapshot(&mut response.rate_limits, &overlay);
    if let Some(snapshot) = response
        .rate_limits_by_limit_id
        .as_mut()
        .and_then(|buckets| buckets.get_mut("codex"))
    {
        crate::mobile_account_status::overlay_snapshot(snapshot, &overlay);
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
    clients: Mutex<HashMap<ConnectionId, Option<u64>>>,
    pub(crate) caption: Mutex<Option<String>>,
}

impl RemoteClientRegistry {
    pub(crate) async fn register(&self, connection_id: ConnectionId, client_name: String) {
        if is_chatgpt_remote_client(Some(client_name.as_str())) {
            self.clients.lock().await.entry(connection_id).or_default();
        }
    }

    pub(crate) async fn push_pool_update_checked(
        &self,
        outgoing: &OutgoingMessageSender,
        pool: &AccountPoolReadResponse,
        is_current: impl Fn() -> bool + Send,
    ) {
        let connection_ids: Vec<_> = self.clients.lock().await.keys().copied().collect();
        push_account_pool_warning(outgoing, &connection_ids, pool, is_current).await;
    }

    pub(crate) async fn unregister(&self, connection_id: ConnectionId) {
        self.clients.lock().await.remove(&connection_id);
    }

    pub(crate) async fn suppress_unattributed_quota(
        &self,
        connection_id: ConnectionId,
        notification: &ServerNotification,
    ) -> bool {
        // Streaming quota updates carry no profile identity. After a concurrent account
        // switch they may belong to an old in-flight request. Mobile must refetch the
        // authenticated snapshot instead of assigning these numbers to the new caption.
        matches!(
            notification,
            ServerNotification::AccountRateLimitsUpdated(_)
        ) && self.clients.lock().await.contains_key(&connection_id)
            && self.caption.lock().await.is_some()
    }
}

pub(crate) fn overlay_get_account_for_remote_client(
    response: &mut GetAccountResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    if let Some(Account::Chatgpt { email, .. }) = response.account.as_mut() {
        // Legacy remote clients receive pool details through this account display field.
        // Keep the quota title short and leave its native progress windows untouched.
        let mut accounts: Vec<_> = pool.accounts.iter().collect();
        accounts.sort_by_key(|account| (!account.is_active, account.priority, &account.profile_id));
        let has_current = accounts.iter().any(|account| account.is_active);
        let visible = accounts.len().min(2);
        let ready = accounts
            .iter()
            .filter(|account| {
                matches!(
                    account.availability,
                    codex_app_server_protocol::AccountPoolAvailability::Available
                )
            })
            .count();
        let mut entries = vec![format!("Pool · {ready}/{} ready", accounts.len())];
        if accounts.is_empty() {
            entries.push("Account pool is empty".into());
        } else if !has_current {
            entries.push("No current account".into());
        }
        for account in accounts.into_iter().take(visible) {
            let name = crate::mobile_account_status::label(account)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let name = crate::mobile_account_status::compact_label(&name, /*max*/ 16);
            let name = if name.is_empty() { "Account" } else { &name };
            let state = match &account.availability {
                codex_app_server_protocol::AccountPoolAvailability::Available => "Ready",
                codex_app_server_protocol::AccountPoolAvailability::Exhausted { .. } => {
                    "Cooling down"
                }
                codex_app_server_protocol::AccountPoolAvailability::AuthenticationUnavailable {
                    ..
                } => "Login required",
                codex_app_server_protocol::AccountPoolAvailability::Disabled => "Disabled",
            };
            let current = if account.is_active { " · Current" } else { "" };
            entries.push(format!("{name}{current} · {state}"));
        }
        let hidden = pool.accounts.len() - visible;
        entries.push(if hidden > 0 {
            format!("+{hidden} more · /account")
        } else {
            "/account".into()
        });
        *email = Some(entries.join(" | "));
    }
}

pub(crate) fn inject_workspace_messages_for_remote_client(
    response: &mut GetWorkspaceMessagesResponse,
    pool: &AccountPoolReadResponse,
) {
    if !pool.enabled {
        return;
    }
    response.feature_enabled = true;
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
    is_current: impl Fn() -> bool + Send,
) {
    if connection_ids.is_empty() {
        return;
    }
    if !pool.enabled {
        let mut clients = outgoing.remote_clients.clients.lock().await;
        for connection_id in connection_ids {
            if let Some(previous) = clients.get_mut(connection_id) {
                *previous = None;
            }
        }
        return;
    }
    let mut hasher = DefaultHasher::new();
    pool.active_profile_id.hash(&mut hasher);
    pool_warning_summary(pool, SummaryPurpose::Deduplication).hash(&mut hasher);
    let fingerprint = hasher.finish();
    let connection_ids: Vec<_> = {
        let mut clients = outgoing.remote_clients.clients.lock().await;
        connection_ids
            .iter()
            .filter_map(|connection_id| {
                let previous = clients.get_mut(connection_id)?;
                if *previous == Some(fingerprint) {
                    return None;
                }
                *previous = Some(fingerprint);
                Some(*connection_id)
            })
            .collect()
    };
    if connection_ids.is_empty() {
        return;
    }
    let notification = ServerNotification::Warning(WarningNotification {
        thread_id: None,
        message: pool_warning_summary(pool, SummaryPurpose::Display),
    });
    if !outgoing
        .send_server_notification_checked(&connection_ids, notification, is_current)
        .await
    {
        let mut clients = outgoing.remote_clients.clients.lock().await;
        for connection_id in &connection_ids {
            if let Some(previous) = clients.get_mut(connection_id)
                && *previous == Some(fingerprint)
            {
                *previous = None;
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum SummaryPurpose {
    Display,
    Deduplication,
}

fn pool_warning_summary(pool: &AccountPoolReadResponse, purpose: SummaryPurpose) -> String {
    let ready = pool
        .accounts
        .iter()
        .filter(|account| {
            matches!(
                account.availability,
                codex_app_server_protocol::AccountPoolAvailability::Available
            )
        })
        .count();
    let mut accounts: Vec<_> = pool.accounts.iter().collect();
    accounts.sort_by_key(|account| {
        (
            Some(account.profile_id.as_str()) != pool.active_profile_id.as_deref(),
            account.priority,
            &account.profile_id,
        )
    });
    let mut entries = vec![format!("Pool · {ready}/{} ready", accounts.len())];
    for account in accounts.iter().take(2) {
        let name = crate::mobile_account_status::compact_label(
            &crate::mobile_account_status::label(account),
            /*max*/ 16,
        );
        let current = if Some(account.profile_id.as_str()) == pool.active_profile_id.as_deref() {
            " · Current"
        } else {
            ""
        };
        let state = match account.availability {
            codex_app_server_protocol::AccountPoolAvailability::Available => "Ready",
            codex_app_server_protocol::AccountPoolAvailability::Exhausted { .. } => "Cooling down",
            codex_app_server_protocol::AccountPoolAvailability::AuthenticationUnavailable {
                ..
            } => "Login required",
            codex_app_server_protocol::AccountPoolAvailability::Disabled => "Disabled",
        };
        let mut entry = format!("{name}{current} · {state}");
        if purpose == SummaryPurpose::Deduplication && !current.is_empty() {
            entries.push(entry);
            continue;
        }
        for (name, window) in [
            ("Primary", &account.rate_limits.primary),
            ("Secondary", &account.rate_limits.secondary),
        ] {
            if let Some(window) = window
                .as_ref()
                .filter(|window| window.used_percent.is_finite())
            {
                let used = window.used_percent.clamp(0.0, 100.0).round() as i32;
                entry.push_str(&format!(" · {name} {used}% used"));
            }
        }
        entries.push(entry);
    }
    if accounts.is_empty() {
        entries.push("Account pool is empty".into());
    } else if pool.active_profile_id.is_none() {
        entries.insert(1, "No current account".into());
    }
    if accounts.len() > 2 {
        entries.push(format!("+{} more", accounts.len() - 2));
    }
    // Limit presentation independently of pool size; fingerprints retain only one
    // word per live mobile connection and ignore observation timestamps.
    format!(
        "{} · /account",
        crate::mobile_account_status::compact_label(&entries.join(" | "), /*max*/ 268)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::AccountPoolAccount;
    use codex_app_server_protocol::AccountPoolAvailability;
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
    fn rate_limits_overlay_marks_the_current_account_quota() {
        let pool = AccountPoolReadResponse {
            enabled: true,
            active_profile_id: Some("primary".to_string()),
            active_generation: Some(1),
            accounts: vec![],
        };
        let mut response = GetAccountRateLimitsResponse {
            ordinary_usage_allowed: None,
            rate_limits: codex_app_server_protocol::RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("Codex".to_string()),
                normal_model_slug: None,
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
                .is_some_and(|name| name == "No current account · Quota")
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
                window_warmup: None,

                backend_resets_at: None,
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
            ordinary_usage_allowed: None,
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
        expected.limit_name = Some("Work · Current quota".to_string());
        assert_eq!(response.rate_limits, expected);
        assert_eq!(
            response.rate_limits_by_limit_id,
            Some(HashMap::from([
                ("codex".to_string(), expected),
                ("other".to_string(), other)
            ]))
        );
        insta::assert_snapshot!(response.rate_limits.limit_name.unwrap(), @"Work · Current quota");
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
                window_warmup: None,

                backend_resets_at: None,
            }],
        };
        let mut response = GetWorkspaceMessagesResponse {
            feature_enabled: false,
            messages: vec![WorkspaceMessage {
                message_id: "backend-1".to_string(),
                message_type: WorkspaceMessageType::Announcement,
                message_body: "Existing headline".to_string(),
                created_at: None,
                archived_at: None,
            }],
        };
        inject_workspace_messages_for_remote_client(&mut response, &pool);
        assert!(response.feature_enabled);
        assert_eq!(response.messages.len(), 2);
        assert_eq!(response.messages[0].message_id, LOCAL_WORKSPACE_MESSAGE_ID);
        assert!(response.messages[0].message_body.contains("Team"));
    }
}

#[cfg(test)]
#[path = "mobile_account_bridge_tests.rs"]
mod ux_tests;
