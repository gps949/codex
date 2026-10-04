//! Owned native question delivery; attached controls preserve the real turn lifecycle.

#[path = "native_account_questions.rs"]
mod questions;
#[cfg(test)]
pub(super) use questions::parse_answer;

use super::*;

enum QuestionFailure {
    Message(String),
    Delivery(String),
}

impl NativeAccountManager {
    /// Opens a nonblocking control request without creating or completing a real turn.
    pub(crate) async fn start_attached(
        self: &Arc<Self>,
        request_id: &ConnectionRequestId,
        thread: Arc<codex_core::CodexThread>,
        target: NativeMenuTarget,
        manager: Arc<AccountManager>,
        outgoing: Arc<OutgoingMessageSender>,
        language: NativeAccountLanguage,
    ) -> Result<(), String> {
        let thread_id = target.thread_id;
        let turn_id = target.turn_id.as_str();
        self.cancel_thread_for_owner(request_id.connection_id, thread_id)
            .await?;
        let inventory = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), manager.inventory())
            .await
            .map_err(|_| "Account inventory timed out".to_string())?
            .map_err(|_| "Account inventory could not be loaded".to_string())?;
        if thread
            .current_turn_environment_selections(turn_id)
            .await
            .is_none()
        {
            return Err("The requested turn is no longer running.".into());
        }
        let (finished_tx, finished) = watch::channel(/*init*/ None);
        let menu = ActiveMenu {
            owner: request_id.connection_id,
            thread_id,
            turn_id: turn_id.into(),
            cancellation: CancellationToken::new(),
            finished,
            kind: MenuKind::Attached(thread),
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.connections.contains_key(&menu.owner)
                || state.menus.contains_key(&thread_id)
                || state.menus.len() >= 32
            {
                return Err(
                    "Another account menu is active or its connection is unavailable.".into(),
                );
            }
            state.menus.insert(thread_id, menu.clone());
        }
        outgoing.record_request_turn_id(request_id, turn_id).await;
        if tokio::time::timeout(
            self.limits.delivery,
            outgoing.send_response(
                request_id.clone(),
                codex_app_server_protocol::TurnSteerResponse {
                    turn_id: turn_id.into(),
                },
            ),
        )
        .await
        .is_err()
        {
            self.remove_menu(thread_id, turn_id);
            return Err("Account menu response could not be delivered.".into());
        }
        let coordinator = Arc::clone(self);
        tokio::spawn(async move {
            let result = coordinator
                .run_attached(
                    &menu,
                    FrozenAccountInventory::from_inventory(inventory),
                    manager,
                    outgoing.as_ref(),
                    language,
                )
                .await;
            finished_tx.send_replace(Some(result));
            coordinator.remove_menu(thread_id, &menu.turn_id);
        });
        Ok(())
    }

    async fn notify(
        &self,
        outgoing: &OutgoingMessageSender,
        owner: ConnectionId,
        notification: ServerNotification,
    ) -> Result<(), String> {
        tokio::time::timeout(
            self.limits.delivery,
            outgoing.send_server_notification_to_connections(&[owner], notification),
        )
        .await
        .map_err(|_| "Account menu notification could not be delivered.".to_string())
    }

    async fn item(
        &self,
        outgoing: &OutgoingMessageSender,
        menu: &ActiveMenu,
        thread_id: ThreadId,
        item: ThreadItem,
    ) -> Result<(), String> {
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        self.notify(
            outgoing,
            menu.owner,
            ServerNotification::ItemStarted(ItemStartedNotification {
                item: item.clone(),
                thread_id: thread_id.to_string(),
                turn_id: menu.turn_id.clone(),
                started_at_ms,
            }),
        )
        .await?;
        self.notify(
            outgoing,
            menu.owner,
            ServerNotification::ItemCompleted(ItemCompletedNotification {
                item,
                thread_id: thread_id.to_string(),
                turn_id: menu.turn_id.clone(),
                completed_at_ms: started_at_ms,
            }),
        )
        .await
    }

    pub(super) async fn run(
        &self,
        menu: &ActiveMenu,
        input: NativeMenuInput,
        manager: Arc<AccountManager>,
        outgoing: &OutgoingMessageSender,
        language: NativeAccountLanguage,
        mut turn: Turn,
    ) -> Result<(), String> {
        let NativeMenuInput { user, inventory } = input;
        let mut result = self
            .notify(
                outgoing,
                menu.owner,
                ServerNotification::TurnStarted(TurnStartedNotification {
                    thread_id: menu.thread_id.to_string(),
                    turn: turn.clone(),
                }),
            )
            .await;
        if result.is_ok() && !menu.cancellation.is_cancelled() {
            result = self.item(outgoing, menu, menu.thread_id, user).await;
        }
        let anchor = menu_anchor(language);
        if result.is_ok() && !menu.cancellation.is_cancelled() {
            result = self
                .item(outgoing, menu, menu.thread_id, anchor.clone())
                .await;
        }
        turn.items = vec![anchor.clone()];
        if result.is_ok() && !menu.cancellation.is_cancelled() {
            result = self
                .run_pages(menu, &anchor, inventory, manager, outgoing, language)
                .await;
        }
        turn.status = if result.is_err() {
            TurnStatus::Failed
        } else if menu.cancellation.is_cancelled() {
            TurnStatus::Interrupted
        } else {
            TurnStatus::Completed
        };
        if result.is_err() {
            turn.error = Some(codex_app_server_protocol::TurnError {
                message: language
                    .text(
                        "Account menu delivery failed. Reopen with /account manage to retry.",
                        "账号菜单发送失败。可用 /account manage 重新打开重试。",
                    )
                    .into(),
                codex_error_info: None,
                additional_details: None,
                misalignment: None,
            });
        }
        turn.items_view = TurnItemsView::Summary;
        turn.completed_at = Some(chrono::Utc::now().timestamp());
        turn.duration_ms = turn
            .started_at
            .map(|at| (chrono::Utc::now().timestamp() - at).max(0) * 1000);
        // A delivered terminal event retires this synthetic menu, never an attached real turn.
        self.notify(
            outgoing,
            menu.owner,
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: menu.thread_id.to_string(),
                turn,
            }),
        )
        .await
    }

    pub(super) async fn run_attached(
        &self,
        menu: &ActiveMenu,
        inventory: FrozenAccountInventory,
        manager: Arc<AccountManager>,
        outgoing: &OutgoingMessageSender,
        language: NativeAccountLanguage,
    ) -> Result<(), String> {
        let MenuKind::Attached(thread) = &menu.kind else {
            return Err("Expected an attached account menu".into());
        };
        let thread = Arc::clone(thread);
        let turn_id = menu.turn_id.clone();
        let cancellation = menu.cancellation.clone();
        let watcher = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(/*millis*/ 500)) => {}
                }
                if thread
                    .current_turn_environment_selections(&turn_id)
                    .await
                    .is_none()
                {
                    cancellation.cancel();
                    break;
                }
            }
        });
        let anchor = menu_anchor(language);
        let result = if menu.cancellation.is_cancelled() {
            Ok(())
        } else {
            self.item(outgoing, menu, menu.thread_id, anchor.clone())
                .await
        };
        let result = if result.is_ok() {
            self.run_pages(menu, &anchor, inventory, manager, outgoing, language)
                .await
        } else {
            result
        };
        watcher.abort();
        result
    }

    async fn run_pages(
        &self,
        menu: &ActiveMenu,
        anchor: &ThreadItem,
        inventory: FrozenAccountInventory,
        manager: Arc<AccountManager>,
        outgoing: &OutgoingMessageSender,
        mut language: NativeAccountLanguage,
    ) -> Result<(), String> {
        let context = match &menu.kind {
            MenuKind::Synthetic => crate::account_management::AccountOperationContext::NativeMenu(
                menu.cancellation.clone(),
            ),
            MenuKind::Attached(thread) => {
                crate::account_management::AccountOperationContext::AttachedMenu {
                    cancellation: menu.cancellation.clone(),
                    thread: Arc::clone(thread),
                    turn_id: menu.turn_id.clone(),
                }
            }
        };
        let mut session = NativeMenuSession::new(inventory);
        session.bind_targets(&manager);
        let deadline = Instant::now() + self.limits.session;
        let mut result = Ok(());
        for _ in 0..40 {
            if menu.cancellation.is_cancelled() || Instant::now() >= deadline {
                break;
            }
            if let MenuKind::Attached(thread) = &menu.kind
                && thread
                    .current_turn_environment_selections(&menu.turn_id)
                    .await
                    .is_none()
            {
                menu.cancellation.cancel();
                break;
            }
            let question = session.question(language);
            let answer = self
                .ask(outgoing, menu, anchor, question, deadline, language)
                .await;
            if menu.cancellation.is_cancelled() || Instant::now() >= deadline {
                break;
            }
            if let MenuKind::Attached(thread) = &menu.kind
                && thread
                    .current_turn_environment_selections(&menu.turn_id)
                    .await
                    .is_none()
            {
                menu.cancellation.cancel();
                break;
            }
            match answer {
                Ok(MenuAnswer::Action(MenuAction::Language)) => {
                    language = language.other();
                    let preferences = crate::account_management::ManagerPreferences {
                        language: match language {
                            NativeAccountLanguage::English => {
                                crate::account_management::ManagerLanguage::English
                            }
                            NativeAccountLanguage::Chinese => {
                                crate::account_management::ManagerLanguage::SimplifiedChinese
                            }
                        },
                    };
                    if let Err(error) = manager.save_preferences(preferences) {
                        session.error(error, language);
                    } else {
                        session.page = MenuPage::Home;
                    }
                }
                Ok(answer) => {
                    let operation_deadline =
                        deadline.min(Instant::now() + Duration::from_secs(/*secs*/ 20));
                    let outcome = tokio::select! {
                        biased;
                        _ = menu.cancellation.cancelled() => break,
                        result = tokio::time::timeout_at(operation_deadline, session.handle(answer, &manager, language, &context)) => result,
                    };
                    match outcome {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => break,
                        Ok(Err(error)) => {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(/*secs*/ 5),
                                session.reload(&manager),
                            )
                            .await;
                            session.error(error, language);
                        }
                        Err(_) => {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(/*secs*/ 5),
                                session.reload(&manager),
                            )
                            .await;
                            session.error(anyhow::anyhow!("Operation timed out; its outcome is unconfirmed. Check current state before retrying."), language);
                        }
                    }
                }
                Err(QuestionFailure::Message(message)) => {
                    // One short error receipt replaces the old full-page chat dump.
                    result = self
                        .item(
                            outgoing,
                            menu,
                            menu.thread_id,
                            ThreadItem::AgentMessage {
                                id: Uuid::now_v7().to_string(),
                                text: message,
                                phase: None,
                                memory_citation: None,
                                delivery: None,
                                questions: None,
                            },
                        )
                        .await;
                    break;
                }
                Err(QuestionFailure::Delivery(error)) => {
                    result = Err(error);
                    break;
                }
            }
        }
        session.close(&manager).await;
        result
    }
}

fn menu_anchor(language: NativeAccountLanguage) -> ThreadItem {
    ThreadItem::AgentMessage {
        id: Uuid::now_v7().to_string(),
        text: language.text("Account controls", "账号管理").into(),
        phase: None,
        memory_citation: None,
        delivery: None,
        questions: None,
    }
}
