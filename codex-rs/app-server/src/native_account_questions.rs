//! Connection-owned question delivery, cancellation and exact captured-answer parsing.

use super::*;

impl NativeAccountManager {
    pub(super) async fn ask(
        &self,
        outgoing: &OutgoingMessageSender,
        menu: &ActiveMenu,
        item: &ThreadItem,
        question: MenuQuestion,
        deadline: Instant,
        language: NativeAccountLanguage,
    ) -> Result<MenuAnswer, QuestionFailure> {
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
                options: (!question.free_text).then(|| {
                    question
                        .choices
                        .iter()
                        .map(|choice| ToolRequestUserInputOption {
                            label: choice.label.clone(),
                            description: choice.description.clone(),
                        })
                        .collect()
                }),
            }],
            is_blocking: matches!(menu.kind, MenuKind::Synthetic(_)),
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
                let action =
                    parse_answer(value, &question_id, &question.choices, question.free_text);
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

pub(in crate::native_account_manager) fn parse_answer(
    value: serde_json::Value,
    question_id: &str,
    choices: &[MenuChoice],
    free_text: bool,
) -> Result<MenuAnswer, ()> {
    let response: ToolRequestUserInputResponse = serde_json::from_value(value).map_err(|_| ())?;
    if response.answers.is_empty() {
        return Ok(MenuAnswer::Action(MenuAction::Close));
    }
    if response.answers.len() != 1 {
        return Err(());
    }
    let answer = response.answers.get(question_id).ok_or(())?;
    if answer.answers.is_empty() {
        return Ok(MenuAnswer::Action(MenuAction::Close));
    }
    let [label] = answer.answers.as_slice() else {
        return Err(());
    };
    if free_text {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 80 || label.chars().any(char::is_control) {
            return Err(());
        }
        if crate::native_account_capabilities::bounded_text(label, /*max_chars*/ 80) != label {
            return Err(());
        }
        return Ok(MenuAnswer::Text(label.into()));
    }
    choices
        .iter()
        .find(|choice| choice.label == *label)
        .map(|choice| MenuAnswer::Action(choice.action))
        .ok_or(())
}
