//! Aligns comparable account fields using display widths across the whole picker.

use super::*;

const COLUMN_COUNT: usize = 5;

pub(super) struct AccountColumnWidths {
    widths: [usize; COLUMN_COUNT],
}

impl AccountColumnWidths {
    pub(super) fn new(accounts: &[AccountPoolAccount], now: DateTime<Utc>) -> Self {
        let mut widths = [0; COLUMN_COUNT];
        for account in accounts {
            for (index, column) in account_description_parts(account, now)
                .iter()
                .take(COLUMN_COUNT)
                .enumerate()
            {
                widths[index] = widths[index].max(column.iter().map(Span::width).sum());
            }
        }
        Self { widths }
    }

    pub(super) fn description(
        &self,
        account: &AccountPoolAccount,
        now: DateTime<Utc>,
    ) -> Vec<Span<'static>> {
        let mut result = Vec::new();
        for (index, mut column) in account_description_parts(account, now)
            .into_iter()
            .enumerate()
        {
            if !result.is_empty() {
                result.push(" · ".dim());
            }
            if index < COLUMN_COUNT {
                let padding =
                    self.widths[index].saturating_sub(column.iter().map(Span::width).sum());
                if index == COLUMN_COUNT - 1 {
                    // Keep the priority label and the right edge of its number in fixed columns.
                    column.insert(1, " ".repeat(padding).dim());
                } else if padding > 0 {
                    column.push(" ".repeat(padding).dim());
                }
            }
            result.extend(column);
        }
        result
    }
}
