use super::worker::parse_answer;
use super::*;
use crate::account_management::AccountManagerInventory;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use codex_analytics::AnalyticsEventsClient;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_protocol::mcp::ClientMcpExtensions;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;

#[test]
fn menu_answers_require_exact_question_and_one_captured_label() {
    let choices = vec![
        ("Show overview".into(), MenuAction::Overview(0)),
        ("Close".into(), MenuAction::Close),
    ];
    for invalid in [
        json!({"answers":{"other":{"answers":["Show overview"]}}}),
        json!({"answers":{"question":{"answers":["Show overview","user_note: switch it"]}}}),
        json!({"answers":{"question":{"answers":["Close"]},"other":{"answers":["Close"]}}}),
        json!({"answers":{"question":{"answers":["switch account"]}}}),
    ] {
        assert_eq!(parse_answer(invalid, "question", &choices), Err(()));
    }
    assert_eq!(
        parse_answer(
            json!({"answers":{"question":{"answers":["Show overview"]}}}),
            "question",
            &choices
        ),
        Ok(MenuAction::Overview(0))
    );
    assert_eq!(
        parse_answer(json!({"answers":{}}), "question", &choices),
        Ok(MenuAction::Close)
    );
}

async fn setup() -> (
    Arc<NativeAccountManager>,
    Arc<OutgoingMessageSender>,
    mpsc::Receiver<OutgoingEnvelope>,
) {
    let coordinator = Arc::new(NativeAccountManager {
        state: Mutex::default(),
        limits: MenuLimits {
            question: Duration::from_secs(/*secs*/ 2),
            session: Duration::from_secs(/*secs*/ 5),
            delivery: Duration::from_secs(/*secs*/ 1),
            finish: Duration::from_secs(/*secs*/ 3),
        },
    });
    let (sender, messages) = mpsc::channel(/*buffer*/ 64);
    let mut outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    outgoing.native_account_manager = Arc::clone(&coordinator);
    let outgoing = Arc::new(outgoing);
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    coordinator.register_connection(
        owner,
        NativeAccountCapabilities::capture(
            "remote",
            "1",
            /*_experimental_api*/ false,
            &ClientMcpExtensions::new([]),
        ),
    );
    (coordinator, outgoing, messages)
}

fn empty_inventory() -> FrozenAccountInventory {
    FrozenAccountInventory::from_inventory(AccountManagerInventory {
        primary_login: None,
        host_now: 1_700_000_000,
        paused: false,
        active_profile_id: None,
        accounts: vec![],
        settings: json!({}),
        login_jobs: vec![],
        api_accounts: vec![],
        api_selection: codex_login::ApiAccountSelection::Subscription,
        api_fallback: codex_login::ApiAccountFallback::default(),
    })
}

async fn launch(
    coordinator: &Arc<NativeAccountManager>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
) {
    coordinator
        .start_inventory(
            &ConnectionRequestId {
                connection_id: ConnectionId(1),
                request_id: RequestId::Integer(10),
            },
            thread_id,
            ThreadItem::UserMessage {
                id: "user-item".into(),
                client_id: None,
                content: vec![],
            },
            empty_inventory(),
            outgoing.clone(),
            NativeAccountLanguage::English,
        )
        .await
        .unwrap();
}

async fn question(
    messages: &mut mpsc::Receiver<OutgoingEnvelope>,
) -> (RequestId, ToolRequestUserInputParams) {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(/*secs*/ 3), messages.recv())
            .await
            .unwrap()
            .unwrap();
        let OutgoingEnvelope::ToConnection {
            connection_id,
            message,
            ..
        } = message
        else {
            panic!("native controls must be targeted");
        };
        assert_eq!(connection_id, ConnectionId(1));
        if let OutgoingMessage::Request(ServerRequest::ToolRequestUserInput {
            request_id: id,
            params,
        }) = message
        {
            return (id, params);
        }
    }
}

async fn answer(
    outgoing: &OutgoingMessageSender,
    request_id: RequestId,
    params: &ToolRequestUserInputParams,
    label: &str,
) {
    outgoing
        .notify_client_response(
            ConnectionId(1),
            request_id,
            json!({"answers":{params.questions[0].id.clone():{"answers":[label]}}}),
        )
        .await;
}

#[tokio::test]
async fn response_precedes_questions_and_native_overview_does_not_enter_core() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    let Some(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::Response(response),
        ..
    }) = messages.recv().await
    else {
        panic!("InProgress response must be queued before worker questions");
    };
    let value = serde_json::to_value(response.result).unwrap();
    assert_eq!(value["turn"]["status"], "inProgress");
    let (id, params) = question(&mut messages).await;
    answer(&outgoing, id, &params, "Show overview").await;
    let (id, params) = question(&mut messages).await;
    assert!(
        params.questions[0]
            .question
            .contains("No enrolled accounts")
    );
    assert_eq!(
        params.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Back", "Close"]
    );
    answer(&outgoing, id, &params, "Close").await;
    loop {
        let Some(OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::AppServerNotification(envelope),
            ..
        }) = messages.recv().await
        else {
            panic!("completion notification expected");
        };
        if let ServerNotification::TurnCompleted(completed) = envelope.notification {
            assert_eq!(completed.turn.status, TurnStatus::Completed);
            break;
        }
    }
    coordinator.cancel_thread(thread_id).await.unwrap();
    assert!(
        coordinator
            .describe(ConnectionId(1), NativeAccountLanguage::English)
            .contains("Answer received")
    );
}

#[tokio::test]
async fn exact_owner_stop_cleans_question_before_completing_and_late_answer_is_ignored() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    let (id, params) = question(&mut messages).await;
    assert!(
        !coordinator
            .try_interrupt(ConnectionId(2), thread_id, &params.turn_id)
            .await
            .unwrap()
    );
    coordinator
        .cancel_thread_for_owner(ConnectionId(2), thread_id)
        .await
        .unwrap();
    assert!(
        !coordinator
            .try_interrupt(ConnectionId(1), thread_id, "wrong-turn")
            .await
            .unwrap()
    );
    assert!(
        coordinator
            .try_interrupt(ConnectionId(1), thread_id, &params.turn_id)
            .await
            .unwrap()
    );
    let mut lifecycle = Vec::new();
    while let Ok(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.try_recv()
    {
        match envelope.notification {
            ServerNotification::ServerRequestResolved(resolved) => {
                assert_eq!(resolved.request_id, id);
                lifecycle.push("resolved");
            }
            ServerNotification::TurnCompleted(completed) => {
                assert_eq!(completed.turn.status, TurnStatus::Interrupted);
                lifecycle.push("completed");
            }
            _ => panic!("no extra item must appear after cancellation"),
        }
    }
    assert_eq!(lifecycle, vec!["resolved", "completed"]);
    answer(&outgoing, id, &params, "Show overview").await;
    assert!(messages.try_recv().is_err());
}

#[tokio::test]
async fn disconnect_cancels_owner_menu_and_reconnect_has_no_inherited_observation() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    question(&mut messages).await;
    outgoing
        .disconnect_connection_owned_scope(ConnectionId(1))
        .await;
    coordinator.unregister(ConnectionId(1)).await.unwrap();
    assert!(
        coordinator
            .describe(ConnectionId(1), NativeAccountLanguage::English)
            .contains("unavailable")
    );
    coordinator.register_connection(
        ConnectionId(2),
        NativeAccountCapabilities::capture(
            "remote",
            "1",
            /*_experimental_api*/ false,
            &ClientMcpExtensions::new([]),
        ),
    );
    assert!(
        coordinator
            .describe(ConnectionId(2), NativeAccountLanguage::English)
            .contains("Not tested")
    );
    assert!(coordinator.state.lock().unwrap().menus.is_empty());
}

#[tokio::test]
async fn unanswered_question_times_out_without_declaring_client_unsupported() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    question(&mut messages).await;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(/*secs*/ 4), messages.recv())
            .await
            .unwrap()
            .unwrap();
        if let OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::AppServerNotification(envelope),
            ..
        } = message
            && matches!(envelope.notification, ServerNotification::TurnCompleted(_))
        {
            break;
        }
    }
    assert!(
        coordinator
            .describe(ConnectionId(1), NativeAccountLanguage::English)
            .contains("No answer received")
    );
    coordinator.cancel_thread(thread_id).await.unwrap();
}

#[tokio::test]
async fn startup_stop_closes_only_the_calling_owner_menu_and_late_stop_does_not_match() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    let (id, params) = question(&mut messages).await;
    assert!(
        !coordinator
            .try_interrupt(ConnectionId(2), thread_id, "")
            .await
            .unwrap()
    );
    assert!(
        coordinator
            .try_interrupt(ConnectionId(1), thread_id, "")
            .await
            .unwrap()
    );
    assert!(
        !coordinator
            .try_interrupt(ConnectionId(1), thread_id, "")
            .await
            .unwrap()
    );
    answer(&outgoing, id, &params, "Show overview").await;
    let mut lifecycle = Vec::new();
    while let Ok(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.try_recv()
    {
        match envelope.notification {
            ServerNotification::ServerRequestResolved(_) => lifecycle.push("resolved"),
            ServerNotification::TurnCompleted(completed) => {
                assert_eq!(completed.turn.status, TurnStatus::Interrupted);
                lifecycle.push("completed");
            }
            _ => panic!("no new menu page should appear after startup Stop"),
        }
    }
    assert_eq!(lifecycle, vec!["resolved", "completed"]);
}

#[tokio::test]
async fn delivered_failure_completion_retires_page_error_without_rejecting_next_turn() {
    let (coordinator, _, _) = setup().await;
    let (sender, mut messages) = mpsc::channel(/*buffer*/ 1);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        sender,
        AnalyticsEventsClient::disabled(),
    ));
    outgoing
        .register_connection_owned_scope(ConnectionId(1))
        .await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    assert!(matches!(
        messages.recv().await,
        Some(OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::Response(_),
            ..
        })
    ));
    assert!(matches!(
        messages.recv().await,
        Some(OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::AppServerNotification(_),
            ..
        })
    ));
    // Leave item/started occupying the queue until item/completed delivery times out.
    tokio::time::sleep(Duration::from_millis(/*millis*/ 1100)).await;
    let Some(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.recv().await
    else {
        panic!("queued item start");
    };
    assert!(matches!(
        envelope.notification,
        ServerNotification::ItemStarted(_)
    ));
    let Some(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.recv().await
    else {
        panic!("failed terminal completion");
    };
    let ServerNotification::TurnCompleted(completed) = envelope.notification else {
        panic!("terminal event expected");
    };
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    assert_eq!(
        completed.turn.error,
        Some(codex_app_server_protocol::TurnError {
            message: "Account menu delivery failed. Reopen with /account manage to retry.".into(),
            codex_error_info: None,
            additional_details: None,
            misalignment: None,
        })
    );
    coordinator.cancel_thread(thread_id).await.unwrap();
    launch(&coordinator, &outgoing, thread_id).await;
    question(&mut messages).await;
    let drain = tokio::spawn(async move {
        while let Some(OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::AppServerNotification(envelope),
            ..
        }) = messages.recv().await
        {
            if matches!(envelope.notification, ServerNotification::TurnCompleted(_)) {
                break;
            }
        }
    });
    coordinator.cancel_thread(thread_id).await.unwrap();
    drain.await.unwrap();
}

#[tokio::test]
async fn undelivered_terminal_blocks_once_until_finished_worker_is_retired() {
    let (mut coordinator, _, _) = setup().await;
    Arc::get_mut(&mut coordinator).unwrap().limits.delivery =
        Duration::from_millis(/*millis*/ 20);
    let (sender, mut messages) = mpsc::channel(/*buffer*/ 1);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        sender,
        AnalyticsEventsClient::disabled(),
    ));
    outgoing
        .register_connection_owned_scope(ConnectionId(1))
        .await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    messages.recv().await.unwrap();
    let mut finished = coordinator
        .state
        .lock()
        .unwrap()
        .menus
        .get(&thread_id)
        .unwrap()
        .finished
        .clone();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 2), async {
        while finished.borrow().is_none() {
            finished.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(finished.borrow().as_ref().unwrap().is_err());
    assert!(coordinator.cancel_thread(thread_id).await.is_err());
    coordinator.cancel_thread(thread_id).await.unwrap();
    while let Ok(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.try_recv()
    {
        assert!(!matches!(
            envelope.notification,
            ServerNotification::TurnCompleted(_)
        ));
    }
    assert!(coordinator.state.lock().unwrap().menus.is_empty());
}

#[tokio::test]
async fn real_turn_barrier_uses_shared_gate_and_completes_native_menu_before_forwarding() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    launch(&coordinator, &outgoing, thread_id).await;
    let (request_id, params) = question(&mut messages).await;
    let ordering = outgoing
        .native_account_ordering
        .lock_thread(thread_id)
        .await;
    let scoped = crate::outgoing_message::ThreadScopedOutgoingMessageSender::new(
        outgoing.clone(),
        vec![ConnectionId(1)],
        thread_id,
    );
    let mut prepare = std::pin::pin!(scoped.prepare_real_turn());
    assert!(futures::poll!(&mut prepare).is_pending());
    assert!(
        !coordinator
            .state
            .lock()
            .unwrap()
            .menus
            .get(&thread_id)
            .unwrap()
            .cancellation
            .is_cancelled()
    );
    drop(ordering);
    let real_ordering = prepare.await;
    scoped
        .send_server_notification(ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread_id.to_string(),
            turn: Turn {
                id: "real-turn".into(),
                items: vec![],
                items_view: TurnItemsView::NotLoaded,
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        }))
        .await;
    drop(real_ordering);
    let mut lifecycle = Vec::new();
    while let Ok(OutgoingEnvelope::ToConnection {
        message: OutgoingMessage::AppServerNotification(envelope),
        ..
    }) = messages.try_recv()
    {
        match envelope.notification {
            ServerNotification::ServerRequestResolved(resolved) => {
                assert_eq!(resolved.request_id, request_id);
                lifecycle.push("resolved");
            }
            ServerNotification::TurnCompleted(completed) => {
                assert_eq!(completed.turn.id, params.turn_id);
                lifecycle.push("native-completed");
            }
            ServerNotification::TurnStarted(started) => {
                assert_eq!(started.turn.id, "real-turn");
                lifecycle.push("real-started");
            }
            _ => panic!("unexpected page after native menu cancellation"),
        }
    }
    assert_eq!(
        lifecycle,
        vec!["resolved", "native-completed", "real-started"]
    );
    answer(&outgoing, request_id, &params, "Show overview").await;
    assert!(messages.try_recv().is_err());
}
