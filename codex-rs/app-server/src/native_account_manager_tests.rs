use super::worker::parse_answer;
use super::*;
use crate::account_management::AccountManagerInventory;
use crate::native_account_view::MenuPage;
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
        MenuChoice {
            label: "Accounts".into(),
            description: String::new(),
            action: MenuAction::Page(MenuPage::Overview(0)),
        },
        MenuChoice {
            label: "Close".into(),
            description: String::new(),
            action: MenuAction::Close,
        },
    ];
    for invalid in [
        json!({"answers":{"other":{"answers":["Accounts"]}}}),
        json!({"answers":{"question":{"answers":["Accounts","user_note: switch it"]}}}),
        json!({"answers":{"question":{"answers":["Close"]},"other":{"answers":["Close"]}}}),
        json!({"answers":{"question":{"answers":["switch account"]}}}),
    ] {
        assert_eq!(
            parse_answer(invalid, "question", &choices, /*free_text*/ false),
            Err(())
        );
    }
    assert_eq!(
        parse_answer(
            json!({"answers":{"question":{"answers":["Accounts"]}}}),
            "question",
            &choices,
            /*free_text*/ false
        ),
        Ok(MenuAnswer::Action(MenuAction::Page(MenuPage::Overview(0))))
    );
    assert_eq!(
        parse_answer(
            json!({"answers":{}}),
            "question",
            &choices,
            /*free_text*/ false
        ),
        Ok(MenuAnswer::Action(MenuAction::Close))
    );
}

pub(super) async fn setup() -> (
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
        decision_advisor: None,
        model_routing: None,
        reset_journals: vec![],
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

pub(super) async fn launch(
    coordinator: &Arc<NativeAccountManager>,
    outgoing: &Arc<OutgoingMessageSender>,
    thread_id: ThreadId,
) -> Arc<codex_core::CodexThread> {
    let home = tempfile::tempdir().unwrap();
    let mut config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .unwrap();
    config.model_catalog = Some(codex_models_manager::bundled_models_response().unwrap());
    let auth = codex_core::test_support::auth_manager_from_auth_with_home(
        codex_login::CodexAuth::from_api_key("synthetic-menu-key"),
        home.path().to_path_buf(),
    );
    let threads = Arc::new(codex_core::ThreadManager::new(
        &config,
        Arc::clone(&auth),
        codex_core::build_models_manager(&config, auth),
        codex_core::CodexAppsToolsCache::default(),
        codex_protocol::protocol::SessionSource::Exec,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        codex_extension_api::empty_extension_registry(),
        Arc::new(codex_core::test_support::EmptyUserInstructionsProvider),
        /*analytics_events_client*/ None,
        codex_core::passthrough_image_store(),
        codex_core::thread_store_from_config(&config, /*state_db*/ None),
        /*agent_graph_store*/ None,
        "11111111-1111-4111-8111-111111111111".into(),
        /*attestation_provider*/ None,
        /*external_time_provider*/ None,
    ));
    let mut options = codex_core::StartThreadOptions::new(config.clone());
    options.reserved_thread_id = Some(thread_id);
    let thread = threads.start_thread(options).await.unwrap().thread;
    let manager = AccountManager::new(config);
    coordinator
        .start_inventory(
            &ConnectionRequestId {
                connection_id: ConnectionId(1),
                request_id: RequestId::Integer(10),
            },
            thread_id,
            NativeMenuInput {
                user: ThreadItem::UserMessage {
                    id: "user-item".into(),
                    client_id: None,
                    content: vec![],
                },
                inventory: empty_inventory(),
                thread: Arc::clone(&thread),
            },
            manager,
            outgoing.clone(),
            NativeMenuOptions {
                entry: NativeMenuEntry::Manage,
                language: NativeAccountLanguage::English,
            },
        )
        .await
        .unwrap();
    let mut finished = coordinator
        .state
        .lock()
        .unwrap()
        .menus
        .get(&thread_id)
        .unwrap()
        .finished
        .clone();
    tokio::spawn(async move {
        while finished.borrow().is_none() {
            if finished.changed().await.is_err() {
                break;
            }
        }
        threads
            .shutdown_all_threads_bounded(Duration::from_secs(/*secs*/ 2))
            .await;
        drop(home);
    });
    thread
}

pub(super) async fn question(
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

pub(super) async fn answer(
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
    answer(&outgoing, id, &params, "Accounts").await;
    let (id, params) = question(&mut messages).await;
    assert!(params.questions[0].question.contains("No accounts yet"));
    assert!(!params.questions[0].question.contains('\n'));
    assert_eq!(
        params.questions[0]
            .options
            .as_ref()
            .unwrap()
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Add subscription account",
            "Reload inventory",
            "Back",
            "Close"
        ]
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
    answer(&outgoing, id, &params, "Accounts").await;
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
    answer(&outgoing, id, &params, "Accounts").await;
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
    answer(&outgoing, request_id, &params, "Accounts").await;
    assert!(messages.try_recv().is_err());
}

#[test]
fn rename_accepts_one_short_free_text_answer_only_on_a_text_page() {
    assert_eq!(
        parse_answer(
            json!({"answers":{"rename":{"answers":["Business seat 2"]}}}),
            "rename",
            &[],
            /*free_text*/ true
        ),
        Ok(MenuAnswer::Text("Business seat 2".into()))
    );
    for text in [
        " ".to_string(),
        "x".repeat(81),
        "bad\nname".into(),
        "bad\u{202e}name".into(),
    ] {
        assert_eq!(
            parse_answer(
                json!({"answers":{"rename":{"answers":[text]}}}),
                "rename",
                &[],
                /*free_text*/ true
            ),
            Err(())
        );
    }
    assert_eq!(
        parse_answer(
            json!({"answers":{"rename":{"answers":["Business seat 2","user_note: select"]}}}),
            "rename",
            &[],
            /*free_text*/ true
        ),
        Err(())
    );
    assert_eq!(
        parse_answer(
            json!({"answers":{"rename":{"answers":["Business seat 2"]}}}),
            "rename",
            &[],
            /*free_text*/ false
        ),
        Err(())
    );
}
