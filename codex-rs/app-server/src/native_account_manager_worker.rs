//! Question delivery, cancellation and synthetic item lifecycle.

use super::*;

enum QuestionFailure {
    Message(String),
    Delivery(String),
}

impl NativeAccountManager {
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
        user: ThreadItem,
        inventory: FrozenAccountInventory,
        outgoing: &OutgoingMessageSender,
        mut language: NativeAccountLanguage,
        mut turn: Turn,
    ) -> Result<(), String> {
        let thread_id = menu.thread_id;
        let deadline = Instant::now() + self.limits.session;
        let started = self
            .notify(
                outgoing,
                menu.owner,
                ServerNotification::TurnStarted(TurnStartedNotification {
                    thread_id: thread_id.to_string(),
                    turn: turn.clone(),
                }),
            )
            .await;
        let mut result = started;
        if result.is_ok() && !menu.cancellation.is_cancelled() {
            result = self.item(outgoing, menu, thread_id, user).await;
        }
        let mut page = MenuPage::Home;
        let mut closing_text = None;
        for _ in 0..12 {
            if result.is_err() || menu.cancellation.is_cancelled() {
                break;
            }
            let question = inventory.question(page, language);
            let item = ThreadItem::AgentMessage {
                id: Uuid::now_v7().to_string(),
                text: question.text.clone(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            };
            result = self.item(outgoing, menu, thread_id, item.clone()).await;
            if result.is_err() || menu.cancellation.is_cancelled() {
                break;
            }
            turn.items = vec![item.clone()];
            let action = self
                .ask(outgoing, menu, &item, question, deadline, language)
                .await;
            match action {
                Ok(MenuAction::Home) => page = MenuPage::Home,
                Ok(MenuAction::ChoosePage { first, end }) => {
                    page = MenuPage::ChoosePage { first, end }
                }
                Ok(MenuAction::Overview(index)) => page = MenuPage::Overview(index),
                Ok(MenuAction::Detail(index)) => page = MenuPage::Detail(index),
                Ok(MenuAction::Language) => {
                    language = language.other();
                    page = MenuPage::Home;
                }
                Ok(MenuAction::Close) => {
                    closing_text = Some(
                        language
                            .text("Account menu closed.", "账号菜单已关闭。")
                            .into(),
                    );
                    break;
                }
                Err(QuestionFailure::Message(message)) => {
                    closing_text = Some(message);
                    break;
                }
                Err(QuestionFailure::Delivery(error)) => {
                    result = Err(error);
                    break;
                }
            }
        }
        if closing_text.is_none() && !menu.cancellation.is_cancelled() && result.is_ok() {
            closing_text = Some(
                language
                    .text(
                        "Menu session finished. Reopen with /account manage to continue.",
                        "菜单会话已结束。可用 /account manage 重新打开继续查看。",
                    )
                    .into(),
            );
        }
        if let Some(text) = closing_text.filter(|_| !menu.cancellation.is_cancelled()) {
            let item = ThreadItem::AgentMessage {
                id: Uuid::now_v7().to_string(),
                text,
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            };
            result = self
                .item(outgoing, menu, thread_id, item.clone())
                .await
                .and(result);
            turn.items = vec![item];
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
        let completed = self
            .notify(
                outgoing,
                menu.owner,
                ServerNotification::TurnCompleted(TurnCompletedNotification {
                    thread_id: thread_id.to_string(),
                    turn,
                }),
            )
            .await;
        // A delivered terminal event retires this menu even when an earlier page failed.
        completed
    }

    async fn ask(
        &self,
        outgoing: &OutgoingMessageSender,
        menu: &ActiveMenu,
        item: &ThreadItem,
        question: MenuQuestion,
        deadline: Instant,
        language: NativeAccountLanguage,
    ) -> Result<MenuAction, QuestionFailure> {
        let thread_id = menu.thread_id;
        let question_id = Uuid::now_v7().to_string();
        let item_id = match item {
            ThreadItem::AgentMessage { id, .. } => id.clone(),
            _ => unreachable!("menu questions attach to their emitted instruction"),
        };
        let payload = ServerRequestPayload::ToolRequestUserInput(ToolRequestUserInputParams {
            thread_id: thread_id.to_string(),
            turn_id: menu.turn_id.clone(),
            item_id,
            questions: vec![ToolRequestUserInputQuestion {
                id: question_id.clone(),
                header: language.text("Accounts", "账号管理").into(),
                question: question.text,
                is_other: false,
                is_secret: false,
                options: Some(
                    question
                        .choices
                        .iter()
                        .map(|(label, _)| ToolRequestUserInputOption {
                            label: label.clone(),
                            description: language
                                .text("Read-only account menu", "只读账号菜单")
                                .into(),
                        })
                        .collect(),
                ),
            }],
            is_blocking: true,
            auto_resolution_ms: None,
        });
        let cancellation = menu.cancellation.child_token();
        let timer_token = cancellation.clone();
        let question_deadline = deadline.min(Instant::now() + self.limits.question);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(question_deadline).await;
            timer_token.cancel();
        });
        let (request_id, response) = outgoing
            .send_connection_owned_request_with_cancellation(
                menu.owner,
                payload,
                thread_id,
                cancellation.clone(),
            )
            .await;
        let response = tokio::select! { biased; _ = cancellation.cancelled() => None, result = response => Some(result) };
        timer.abort();
        outgoing.cancel_request(&request_id).await;
        self.notify(
            outgoing,
            menu.owner,
            ServerNotification::ServerRequestResolved(ServerRequestResolvedNotification {
                thread_id: thread_id.to_string(),
                request_id,
            }),
        )
        .await
        .map_err(QuestionFailure::Delivery)?;
        match response {
            Some(Ok(Ok(value))) => {
                let action = parse_answer(value, &question_id, &question.choices);
                self.observe(
                    menu.owner,
                    if action.is_ok() {
                        QuestionObservation::Responded
                    } else {
                        QuestionObservation::InvalidAnswer
                    },
                );
                action.map_err(|_| QuestionFailure::Message(language.text("Answer not accepted. Reopen with /account manage and choose one option.", "未接受此回复。请用 /account manage 重新打开，并选择一个选项。").into()))
            }
            Some(Ok(Err(error))) if error.code == -32601 => {
                self.observe(menu.owner, QuestionObservation::MethodNotFound);
                Err(QuestionFailure::Message(language.text("This connection did not handle native questions. Try /account list for account status.", "此连接未处理原生问答。可用 /account list 查看账号状态。").into()))
            }
            Some(_) => {
                self.observe(menu.owner, QuestionObservation::Rejected);
                Err(QuestionFailure::Message(language.text("Account question closed or connection lost. Reopen with /account manage.", "账号问答已关闭或连接已断开。可用 /account manage 重新打开。").into()))
            }
            None => {
                if !menu.cancellation.is_cancelled() {
                    self.observe(menu.owner, QuestionObservation::NoAnswer);
                }
                Err(QuestionFailure::Message(
                    language
                        .text(
                            "No answer received. Reopen with /account manage when ready.",
                            "未收到回复。准备好后可用 /account manage 重新打开。",
                        )
                        .into(),
                ))
            }
        }
    }
}

pub(super) fn parse_answer(
    value: serde_json::Value,
    question_id: &str,
    choices: &[(String, MenuAction)],
) -> Result<MenuAction, ()> {
    let response: ToolRequestUserInputResponse = serde_json::from_value(value).map_err(|_| ())?;
    if response.answers.is_empty() {
        return Ok(MenuAction::Close);
    }
    if response.answers.len() != 1 {
        return Err(());
    }
    let answer = response.answers.get(question_id).ok_or(())?;
    if answer.answers.is_empty() {
        return Ok(MenuAction::Close);
    }
    let [label] = answer.answers.as_slice() else {
        return Err(());
    };
    choices
        .iter()
        .find(|(known, _)| known == label)
        .map(|(_, action)| *action)
        .ok_or(())
}
