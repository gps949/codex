//! Current account-pool policies displayed in the `/account` Help tab.

use codex_config::AccountPoolConfigToml;
use codex_config::AutoResetCredits;

use crate::bottom_pane::SelectionItem;

pub(super) fn items(config: &AccountPoolConfigToml) -> Vec<SelectionItem> {
    let switch_policy = match config.effective_preemptive_switch_percent() {
        Some(percent) => format!(
            "Switch near {percent:.0}% used. Keep the remaining quota in reserve and use it when other accounts are empty."
        ),
        None => "Switch after a usage limit. Use all eligible accounts before waiting.".to_string(),
    };
    let wait = config.effective_reset_wait();
    let wait_policy = if wait.is_zero() {
        "Automatic waiting is off. Retry after quota recovers.".to_string()
    } else {
        format!(
            "Wait up to {} min for quota recovery and continue when safe. Cancel the turn to stop waiting.",
            wait.as_secs() / 60
        )
    };
    let warmup_policy = if config.effective_window_warmup() {
        format!(
            "Check one standby every {} min after a 30s startup wait. Small requests use quota; 0% means start unconfirmed. See /warmup for details.",
            config.effective_window_warmup_interval().as_secs() / 60
        )
    } else {
        "Off. Standby windows start when you first use the account.".to_string()
    };
    let credits_policy = match config.effective_auto_reset_credits() {
        AutoResetCredits::Never => {
            "Manual only. Check /status for available reset credits.".to_string()
        }
        AutoResetCredits::WhenPoolExhausted => format!(
            "Redeem automatically only when the pool is empty and a natural reset is more than {} min away.",
            config.effective_reset_credit_min_wait_minutes()
        ),
    };
    [
        ("Reserve quota", switch_policy),
        ("Wait and resume", wait_policy),
        ("Standby warmup", warmup_policy),
        ("Reset credits", credits_policy),
        (
            "Change settings",
            "Use `codex account config --help` to control warmup, waiting, and automatic credit use."
                .to_string(),
        ),
        (
            "Manage accounts",
            "Use `codex account --help` to add, relogin, label, enable, or disable profiles."
                .to_string(),
        ),
    ]
    .into_iter()
    .map(|(name, description)| SelectionItem {
        name: name.to_string(),
        search_value: Some(format!("{name} {description}")),
        description: Some(description),
        ..Default::default()
    })
    .collect()
}
