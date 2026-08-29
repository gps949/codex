use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Default usage percentage that triggers a preemptive account rotation.
const DEFAULT_PREEMPTIVE_SWITCH_PERCENT: f64 = 95.0;

/// Default minimum distance to the next natural quota reset before a reset
/// credit is worth redeeming automatically.
const DEFAULT_RESET_CREDIT_MIN_WAIT_MINUTES: i64 = 60;

/// When the scheduler may redeem an earned rate-limit reset credit on the
/// user's behalf. Credits are a limited resource, so automation is opt-in.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoResetCredits {
    /// Never redeem automatically (default). Redemption stays an explicit
    /// user action (TUI /status, app UI, `account/rateLimitResetCredit/consume`).
    #[default]
    Never,
    /// Redeem one credit only when every configured account is exhausted and
    /// the earliest natural reset is still further away than
    /// `auto_reset_credit_min_wait_minutes`.
    WhenPoolExhausted,
}

/// How the scheduler picks the next eligible account when the active profile is unavailable
/// or the user selects automatic scheduling.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountPoolRotationStrategy {
    /// Prefer the lowest `priority` value among eligible profiles (default).
    #[default]
    FillFirst,
    /// Prefer the eligible profile whose observed rate-limit window resets soonest.
    EarliestReset,
}

/// Scheduling knobs for the native multi-account execution pool.
///
/// The pool itself is enabled by the account-profile manifest created with `codex account add`;
/// this section only tunes how the scheduler behaves once more than one profile exists.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AccountPoolConfigToml {
    /// Rotate the active account before a hard usage-limit failure once its observed usage for
    /// any rate-limit window reaches this percentage. Defaults to 95. Values outside the
    /// exclusive (0, 100) range disable preemptive switching.
    pub preemptive_switch_percent: Option<f64>,
    /// Return to the most preferred (lowest priority value) account when its quota cooldown
    /// expires instead of staying on the currently active account. Defaults to true.
    pub return_to_preferred: Option<bool>,
    /// Automatic account selection strategy when failover or `/account` automatic mode runs.
    /// Defaults to `fill_first`.
    pub rotation_strategy: Option<AccountPoolRotationStrategy>,
    /// When the scheduler may automatically redeem an earned rate-limit reset credit.
    /// Defaults to `never`: some users prefer waiting out a nearby natural reset or saving
    /// credits for a broader account-wide reset.
    pub auto_reset_credits: Option<AutoResetCredits>,
    /// With `auto_reset_credits = "when_pool_exhausted"`, skip redemption when the earliest
    /// natural reset across the pool is within this many minutes (waiting is free). Defaults
    /// to 60.
    pub auto_reset_credit_min_wait_minutes: Option<i64>,
}

impl AccountPoolConfigToml {
    /// Effective preemptive switch threshold; `None` means the feature is disabled.
    pub fn effective_preemptive_switch_percent(&self) -> Option<f64> {
        let percent = self
            .preemptive_switch_percent
            .unwrap_or(DEFAULT_PREEMPTIVE_SWITCH_PERCENT);
        (percent > 0.0 && percent < 100.0).then_some(percent)
    }

    pub fn effective_return_to_preferred(&self) -> bool {
        self.return_to_preferred.unwrap_or(true)
    }

    pub fn effective_rotation_strategy(&self) -> AccountPoolRotationStrategy {
        self.rotation_strategy.unwrap_or_default()
    }

    pub fn effective_auto_reset_credits(&self) -> AutoResetCredits {
        self.auto_reset_credits.unwrap_or_default()
    }

    pub fn effective_reset_credit_min_wait_minutes(&self) -> i64 {
        self.auto_reset_credit_min_wait_minutes
            .unwrap_or(DEFAULT_RESET_CREDIT_MIN_WAIT_MINUTES)
            .max(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn rotation_strategy_defaults_to_fill_first() {
        assert_eq!(
            AccountPoolConfigToml::default().effective_rotation_strategy(),
            AccountPoolRotationStrategy::FillFirst
        );
    }

    #[test]
    fn preemptive_switch_defaults_to_95_percent() {
        assert_eq!(
            AccountPoolConfigToml::default().effective_preemptive_switch_percent(),
            Some(95.0)
        );
    }

    #[test]
    fn out_of_range_percent_disables_preemptive_switch() {
        for percent in [0.0, -1.0, 100.0, 250.0] {
            let config = AccountPoolConfigToml {
                preemptive_switch_percent: Some(percent),
                ..Default::default()
            };
            assert_eq!(config.effective_preemptive_switch_percent(), None);
        }
    }
}
