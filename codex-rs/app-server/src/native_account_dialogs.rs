//! Short confirmation, result, credit and login pages for native controls.

use super::*;

impl NativeMenuSession {
    pub(crate) fn question(&self, language: NativeAccountLanguage) -> MenuQuestion {
        let back = |page| choice(language.text("Back", "返回"), "", MenuAction::Page(page));
        match self.page {
            MenuPage::Confirm => {
                let Some(pending) = &self.pending else {
                    return self.inventory.question(MenuPage::Home, language);
                };
                MenuQuestion::new(
                    &pending.title,
                    vec![
                        choice(
                            language.text("Confirm", "确认"),
                            &pending.description,
                            MenuAction::Apply,
                        ),
                        choice(
                            language.text("Cancel", "取消"),
                            language.text("No change will be made", "不会执行此操作"),
                            MenuAction::Page(pending.return_page),
                        ),
                    ],
                )
            }
            MenuPage::Result => MenuQuestion::new(
                language.text("Operation result", "操作结果"),
                vec![
                    choice(
                        language.text("Continue", "继续"),
                        receipt(&self.notice, language),
                        MenuAction::Page(self.return_page),
                    ),
                    choice(language.text("Close", "关闭"), "", MenuAction::Close),
                ],
            ),
            MenuPage::Credits(index, page) => {
                if let Some(previous) = &self.pending_reset {
                    let mut choices = vec![choice(language.text("Previous reset", "之前的重置"),
                        language.text("Its result is unconfirmed. Inspect or retry this operation before using another credit.", "结果尚未确认；使用另一张券前请核验或重试这次操作。"), MenuAction::Page(self.page))];
                    if self.credit_inventory_error.is_some() {
                        choices[0].description.push_str(language.text(
                            "\nCredit inventory unavailable; current count unknown. The original operation is retained.",
                            "\n重置券库存不可用，当前数量未知；原操作仍保留。",
                        ));
                    }
                    if previous.credit_id.is_some() {
                        choices.push(choice(language.text("Retry previous reset", "重试之前的重置"),
                            language.text("Requires confirmation; preserves the original credit and operation key", "需要确认；保留原券与原操作键"),
                            MenuAction::Prepare(MenuOperation::RetryPendingCredit(index))));
                    }
                    choices.extend([
                        choice(
                            language.text("Reload credits", "重载重置券"),
                            language.text(
                                "Recheck this account's reset operation",
                                "重新核验此账号的重置操作",
                            ),
                            MenuAction::Execute(MenuOperation::Credits(index)),
                        ),
                        back(MenuPage::Actions(index)),
                    ]);
                    return MenuQuestion::new(
                        language.text("Reset needs confirmation", "重置待确认"),
                        choices,
                    );
                }
                let mut choices = self
                    .credits
                    .iter()
                    .enumerate()
                    .skip(page * 3)
                    .take(3)
                    .map(|(credit_index, credit)| {
                        choice(
                            &format!("{} {}", language.text("Credit", "重置券"), credit_index + 1),
                            format!(
                                "{}\n{}: {}",
                                if credit.available {
                                    language.text(
                                        "Available · resets Codex quota",
                                        "可用 · 重置 Codex 额度",
                                    )
                                } else {
                                    language.text(
                                        "Unavailable; refresh the list",
                                        "不可使用；请刷新列表",
                                    )
                                },
                                language.text("Expires", "到期"),
                                credit.expires
                            ),
                            MenuAction::Prepare(MenuOperation::Redeem(index, credit_index)),
                        )
                    })
                    .collect::<Vec<_>>();
                if self.credits.len() > 3 {
                    choices.push(choice(
                        language.text("Next credits", "下一页券"),
                        "",
                        MenuAction::Page(MenuPage::Credits(
                            index,
                            (page + 1) % self.credits.len().div_ceil(3),
                        )),
                    ));
                }
                choices.push(back(MenuPage::Actions(index)));
                MenuQuestion::new(
                    if self.credits.is_empty() {
                        language.text("No reset credits", "没有重置券")
                    } else {
                        language.text("Choose a reset credit", "选择重置券")
                    },
                    choices,
                )
            }
            MenuPage::Login => {
                let mut choices = Vec::new();
                if let Some(login) = &self.login {
                    choices.push(choice(
                        language.text("Check sign-in progress", "检查登录进度"),
                        format!(
                            "{}\n{}\n{} {}",
                            receipt(&login.message, language),
                            login.verification_url.as_deref().unwrap_or(""),
                            language.text("Code:", "验证码："),
                            login.user_code.as_deref().unwrap_or("")
                        ),
                        MenuAction::Execute(MenuOperation::LoginCheck),
                    ));
                    if login.status == "waiting" {
                        choices.push(choice(language.text("Cancel sign-in", "取消登录"), language.text("Cancel this sign-in. Unfinished new enrollment is removed; an existing account is kept.", "取消此登录；清理尚未完成的新账号注册，保留已有账号。"), MenuAction::Execute(MenuOperation::LoginCancel)));
                    }
                }
                choices.push(back(MenuPage::Home));
                MenuQuestion::new(
                    language.text("Complete browser sign-in", "完成浏览器登录"),
                    choices,
                )
            }
            page => {
                let mut question = self.inventory.question(page, language);
                if matches!(page, MenuPage::Detail(_)) {
                    for choice in &mut question.choices {
                        if choice.label == language.text("Back", "返回") {
                            choice.action = MenuAction::Page(self.list_origin.page());
                        }
                    }
                }
                question
            }
        }
    }
}

pub(super) fn receipt(message: &str, language: NativeAccountLanguage) -> &str {
    let translated = match message {
        "Account selected for subsequent requests." => "已选择此账号，后续请求将使用它。",
        "Local quota cooldown cleared for one probe. No reset credit was used." => {
            "已清除本地额度冷却，可尝试一次；未使用重置券。"
        }
        "Returned to subscription account selection. Exhausted accounts retain their cooldowns." => {
            "已恢复使用订阅账号池；耗尽账号保留冷却状态。"
        }
        "Account details saved. Running clients synchronize the change." => {
            "已保存账号信息；运行中的客户端会同步变更。"
        }
        "Account removed from the pool. Credentials were retained." => {
            "已从账号池移除；本地凭据已保留。"
        }
        "Account removed from the pool and local credentials deleted. Server token revocation was attempted." => {
            "已从账号池移除并删除本地凭据；已尝试撤销服务端令牌。"
        }
        "Pool settings saved. Active sessions apply them after configuration refresh." => {
            "已保存账号池设置；活动会话将在配置刷新后应用。"
        }
        "Host sign-in selected. Running hosts apply this automatically and continue Remote if it was enabled. Devices may need pairing for the new owner." => {
            "已选择主登录账号；主机会自动应用并继续已开启的 Remote。设备可能需要为新身份重新配对。"
        }
        "Host sign-in now uses root login. Inference selection is unchanged." => {
            "主登录已改用根登录；推理账号选择不变。"
        }
        "Host signed out. Pool credentials were retained." => "主登录已退出；账号池凭据已保留。",
        "API account saved for manual selection. No generating request was sent." => {
            "已保存 API 账号，可手动选择；未发送推理请求。"
        }
        "API account details updated." => "已更新 API 账号信息。",
        "API key replaced for subsequent turns. No generating request was sent." => {
            "已更换 API 密钥，后续轮次将使用新密钥；未发送推理请求。"
        }
        "Manual API target selected for subsequent turns. Usage is billed by this provider." => {
            "已选择手动 API 目标，后续轮次将使用它；使用由此提供商计费。"
        }
        "API account and its local key removed." => "已移除 API 账号及本地密钥。",
        "API fallback policy saved. Automatic subscription selection remains the default." => {
            "已保存 API 兜底设置；默认仍自动选择订阅账号。"
        }
        "Reset credits loaded for the selected account." => "已读取所选账号的重置券。",
        "Reset credit redeemed and quota recovery confirmed for this account." => {
            "已兑换重置券，并确认此账号的额度恢复。"
        }
        "Reset credit redeemed. Checking fresh quota before confirming local recovery." => {
            "已兑换重置券；正在查询最新额度以确认本地恢复。"
        }
        "Requesting browser verification code" => "正在请求浏览器验证码",
        "Waiting for browser verification" => "等待浏览器验证",
        "Login cancellation requested." => "已请求取消登录。",
        "Login cancelled" => "登录已取消",
        _ => return message,
    };
    match language {
        NativeAccountLanguage::English => message,
        NativeAccountLanguage::Chinese => translated,
    }
}
