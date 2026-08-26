use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Default usage percentage that triggers a preemptive account rotation.
const DEFAULT_PREEMPTIVE_SWITCH_PERCENT: f64 = 95.0;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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
                return_to_preferred: None,
            };
            assert_eq!(config.effective_preemptive_switch_percent(), None);
        }
    }
}
