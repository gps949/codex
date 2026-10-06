//! Explicit review keeps unknown reset outcomes bound to their original operation.

use super::*;

const RESET_PAGE_SIZE: usize = 3;

fn record_summary(
    record: &crate::account_management::ResetJournalView,
    language: NativeAccountLanguage,
) -> String {
    if let Some(manual) = &record.manual {
        format!(
            "{}: {}\n{}: {}\n{}",
            language.text("Operation", "操作编号"),
            bounded_text(&manual.idempotency_key, /*max_chars*/ 100),
            language.text("Credit", "重置券"),
            bounded_text(&manual.credit_id, /*max_chars*/ 100),
            language.text(
                "Outcome unknown; original account may be unavailable",
                "结果未知；原账号可能已不可访问"
            )
        )
    } else {
        format!(
            "{}\n{}: {}",
            match record.message.as_str() {
                "Owner-bound reset is awaiting confirmed recovery." =>
                    language.text("Waiting for confirmed recovery", "等待确认额度恢复"),
                "Legacy reset attempt needs review." =>
                    language.text("Legacy reset needs review", "旧版重置待复核"),
                "Damaged reset record needs review." =>
                    language.text("Damaged reset record", "重置记录损坏"),
                "Unknown reset record needs manual review." =>
                    language.text("Unknown reset format", "未知重置记录格式"),
                "Manual reset records could not be inspected. No new automatic credit will be spent." =>
                    language.text(
                        "Manual reset records unreadable; automatic credits remain paused",
                        "无法读取手动重置记录；自动用券继续暂缓"
                    ),
                _ => language.text("Host review required", "需要在主机复核"),
            },
            language.text("Account", "账号"),
            bounded_text(
                record
                    .profile_id
                    .as_deref()
                    .unwrap_or(language.text("Unknown or removed", "未知或已移除")),
                /*max_chars*/ 100
            )
        )
    }
}

impl FrozenAccountInventory {
    pub(in crate::native_account_view) fn reset_review_question(
        &self,
        page: MenuPage,
        language: NativeAccountLanguage,
    ) -> MenuQuestion {
        let back = |page| choice(language.text("Back", "返回"), "", MenuAction::Page(page));
        if let MenuPage::ResetRecord(index) = page {
            let Some(record) = self.reset_journals.get(index) else {
                return self.reset_review_question(MenuPage::ResetReviews(0), language);
            };
            return MenuQuestion::new(language.text("Reset record", "重置记录"), vec![
                choice(language.text("Reload records", "重载记录"), format!("{}\n{}", record_summary(record, language), language.text("This record cannot be reviewed here. Check quota and reset history on the host.", "此记录不能在这里复核。请在主机核对额度与重置历史。")), MenuAction::Execute(MenuOperation::Reload)),
                back(MenuPage::ResetReviews(index / RESET_PAGE_SIZE)),
            ]);
        }
        let MenuPage::ResetReviews(page) = page else {
            unreachable!("reset review page");
        };
        let pages = self.reset_journals.len().div_ceil(RESET_PAGE_SIZE).max(1);
        let page = page.min(pages - 1);
        let mut choices = self
            .reset_journals
            .iter()
            .enumerate()
            .skip(page * RESET_PAGE_SIZE)
            .take(RESET_PAGE_SIZE)
            .map(|(index, record)| {
                choice(
                    &format!(
                        "{}. {}",
                        index + 1,
                        language.text("Unconfirmed reset", "未确认的重置")
                    ),
                    record_summary(record, language),
                    if record.manual.is_some() || record.archive_available {
                        MenuAction::Prepare(MenuOperation::ResetReview(index))
                    } else {
                        MenuAction::Page(MenuPage::ResetRecord(index))
                    },
                )
            })
            .collect::<Vec<_>>();
        if pages > 1 {
            choices.push(choice(
                language.text("Next records", "下一页记录"),
                format!("{}/{}", page + 1, pages),
                MenuAction::Page(MenuPage::ResetReviews((page + 1) % pages)),
            ));
        }
        choices.push(choice(
            language.text("Reload records", "重载记录"),
            if self.reset_total > self.reset_journals.len() {
                language.text(
                    "Showing first 128. Review and reload to reach more records.",
                    "先显示前 128 条。复核并重载后可继续查看后续记录。",
                )
            } else {
                language.text(
                    "Read current operation state; no reset credit is used",
                    "读取当前操作状态；不会使用重置券",
                )
            },
            MenuAction::Execute(MenuOperation::Reload),
        ));
        choices.push(back(MenuPage::Home));
        MenuQuestion::new(
            if choices.len() == 2 {
                language.text("No interrupted resets", "没有中断的重置")
            } else {
                language.text("Interrupted resets", "中断的重置")
            },
            choices,
        )
    }
}

impl NativeMenuSession {
    pub(in crate::native_account_view) fn prepare_reset_review(
        &mut self,
        index: usize,
        language: NativeAccountLanguage,
    ) -> anyhow::Result<()> {
        let record = self
            .inventory
            .reset_journals
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("Reset record no longer appears; reload records"))?;
        let operation = if let Some(manual) = &record.manual {
            AccountManagerOperation::ManualResetReview {
                owner_key: manual.owner_key.clone(),
                idempotency_key: manual.idempotency_key.clone(),
                expected_digest: manual.digest.clone(),
                acknowledge_unconfirmed: true,
            }
        } else {
            anyhow::ensure!(
                record.archive_available,
                "This record cannot be archived safely. Check quota and reset history on the host."
            );
            AccountManagerOperation::ResetJournalArchive {
                file_name: record.file_name.clone(),
                expected_digest: record.digest.clone(),
                acknowledge_unconfirmed: true,
            }
        };
        let return_page = MenuPage::ResetReviews(index / RESET_PAGE_SIZE);
        let description = format!("{}\n{}\n{}\n{}", record_summary(record, language),
            language.text("Check quota and credit history first; the credit may already be consumed.", "请先核对额度与用券历史；这张券可能已被消耗。"),
            language.text("Keep the original binding and a backup; outcome stays unknown. Later operations may use another credit.", "保留原操作绑定和备份，结果仍未知；后续操作可能消耗另一张券。"),
            language.text("Quota and account access are unchanged.", "不会改变额度或账号访问权限。"));
        self.pending = Some(PendingOperation {
            operation,
            target: None,
            expected_identity: None,
            title: language.text("Review this reset?", "复核此次重置？").into(),
            description,
            return_page,
            pending_reset: None,
        });
        self.return_page = return_page;
        self.page = MenuPage::Confirm;
        Ok(())
    }
}

pub(super) fn validate_reset_review(
    operation: &AccountManagerOperation,
    captured: &FrozenAccountInventory,
    fresh: &FrozenAccountInventory,
) -> anyhow::Result<()> {
    let expected = match operation {
        AccountManagerOperation::ManualResetReview {
            owner_key,
            idempotency_key,
            expected_digest,
            acknowledge_unconfirmed,
        } => {
            anyhow::ensure!(
                *acknowledge_unconfirmed,
                "Reset review requires explicit acknowledgement"
            );
            captured.reset_journals.iter().find(|record| {
                record.manual.as_ref().is_some_and(|manual| {
                    &manual.owner_key == owner_key
                        && &manual.idempotency_key == idempotency_key
                        && &manual.digest == expected_digest
                })
            })
        }
        AccountManagerOperation::ResetJournalArchive {
            file_name,
            expected_digest,
            acknowledge_unconfirmed,
        } => {
            anyhow::ensure!(
                *acknowledge_unconfirmed,
                "Reset archive requires explicit acknowledgement"
            );
            captured.reset_journals.iter().find(|record| {
                record.manual.is_none()
                    && record.archive_available
                    && &record.file_name == file_name
                    && &record.digest == expected_digest
            })
        }
        _ => anyhow::bail!("Expected an explicit reset review operation"),
    }
    .ok_or_else(|| anyhow::anyhow!("Original reset review snapshot is unavailable"))?;
    let expected = serde_json::to_value(expected)?;
    anyhow::ensure!(
        fresh
            .reset_journals
            .iter()
            .any(|record| serde_json::to_value(record).is_ok_and(|record| record == expected)),
        "Reset record changed. Reload and review its original operation before confirming."
    );
    Ok(())
}

#[cfg(test)]
#[path = "native_account_reset_review_tests.rs"]
mod tests;
