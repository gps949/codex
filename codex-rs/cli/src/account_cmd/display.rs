use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountProfileState;
use codex_login::AccountRateLimits;
use serde::Serialize;

use super::display_table::percentage;
use super::display_table::render_table;
use super::display_table::sanitize;
use super::quota_display;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum AccountOutputFormat {
    /// Use a readable table on terminals and TSV when redirected.
    #[default]
    Auto,
    /// Render a readable overview sized to the terminal.
    Table,
    /// Preserve the tab-separated columns used by scripts.
    Tsv,
    /// Include complete, nonsecret inventory and cached quota metadata.
    Json,
}

#[derive(Clone, Copy)]
pub(crate) struct AccountOutputOptions {
    pub format: AccountOutputFormat,
    pub show_profile: bool,
    pub details: bool,
}

#[derive(Clone, Copy)]
pub(super) enum AccountView {
    List,
    Pool,
}

#[derive(Clone, Copy)]
pub(super) enum OutputDestination {
    Terminal,
    Pipe,
}

impl AccountOutputFormat {
    pub(super) fn resolve(self, destination: OutputDestination) -> Self {
        match self {
            Self::Auto => match destination {
                OutputDestination::Terminal => Self::Table,
                OutputDestination::Pipe => Self::Tsv,
            },
            Self::Table | Self::Tsv | Self::Json => self,
        }
    }
}

#[derive(Serialize)]
pub(super) struct AccountInventory {
    pub generated_at: i64,
    pub suspended: bool,
    pub warnings: Vec<String>,
    pub settings: Option<PoolSettings>,
    pub accounts: Vec<AccountRow>,
}

impl AccountInventory {
    pub(super) fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.generated_at, /*nsecs*/ 0).unwrap_or_default()
    }
}

#[derive(Serialize)]
pub(super) struct PoolSettings {
    pub rotation_strategy: String,
    pub return_to_preferred: bool,
    pub preemptive_switch: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoginState {
    Cached,
    Pending,
    Missing,
    ReadFailed,
}

impl LoginState {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Pending => "pending",
            Self::Missing => "missing",
            Self::ReadFailed => "read failed",
        }
    }
}

#[derive(Clone, Serialize)]
pub(super) struct AccountRow {
    pub profile_id: String,
    pub label: Option<String>,
    pub active: bool,
    pub priority: u32,
    pub state: AccountProfileState,
    pub disabled: bool,
    pub login: LoginState,
    pub availability: String,
    pub plan: Option<String>,
    pub email: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub rate_limits: AccountRateLimits,
    pub warmup: Option<String>,
}

pub(super) fn render(
    inventory: &AccountInventory,
    view: AccountView,
    options: AccountOutputOptions,
    columns: usize,
) -> String {
    match options.format {
        AccountOutputFormat::Json => {
            // AccountInventory deliberately omits credential paths and auth material.
            match serde_json::to_string_pretty(inventory) {
                Ok(json) => json + "\n",
                Err(error) => serde_json::json!({"error":format!("Inventory could not be serialized: {error}")}).to_string() + "\n",
            }
        }
        AccountOutputFormat::Tsv => render_tsv(inventory, view, options),
        AccountOutputFormat::Auto | AccountOutputFormat::Table => {
            render_table(inventory, view, options, columns.max(/*other*/ 1))
        }
    }
}

fn render_tsv(
    inventory: &AccountInventory,
    view: AccountView,
    options: AccountOutputOptions,
) -> String {
    let mut lines = Vec::new();
    if let Some(settings) = &inventory.settings {
        lines.push(format!(
            "rotation_strategy={}\treturn_to_preferred={}\tpreemptive_switch={}",
            settings.rotation_strategy, settings.return_to_preferred, settings.preemptive_switch,
        ));
    }
    let mut header = vec!["ACTIVE", "PRIORITY"];
    if options.show_profile {
        header.push("PROFILE");
    }
    header.extend(match view {
        AccountView::List => vec!["STATE", "PLAN", "EMAIL", "COOLDOWN", "LABEL"],
        AccountView::Pool => vec![
            "AVAILABILITY",
            "PLAN",
            "EMAIL",
            "5H%",
            "WEEK%",
            "WARMUP",
            "LABEL",
            "PRIMARY_OBSERVED",
            "SECONDARY_OBSERVED",
            "PRIMARY_RESET",
            "SECONDARY_RESET",
        ],
    });
    lines.push(header.join("\t"));
    let now = inventory.now();
    for row in &inventory.accounts {
        let mut cells = vec![
            if row.active {
                "*".into()
            } else {
                String::new()
            },
            row.priority.to_string(),
        ];
        if options.show_profile {
            cells.push(row.profile_id.clone());
        }
        let plan = row.plan.as_deref().unwrap_or("-").to_string();
        let email = row.email.as_deref().unwrap_or("-").to_string();
        let label = row.label.as_deref().unwrap_or("-").to_string();
        match view {
            AccountView::List => cells.extend([
                if row.disabled {
                    "disabled"
                } else {
                    match row.state {
                        AccountProfileState::PendingLogin => "pending_login",
                        AccountProfileState::Ready => "ready",
                    }
                }
                .to_string(),
                plan,
                email,
                row.cooldown_until
                    .map(codex_login::format_exhausted_reset)
                    .unwrap_or_else(|| "-".into()),
                label,
            ]),
            AccountView::Pool => cells.extend([
                if row.availability == "cooldown" {
                    row.cooldown_until
                        .map(|until| {
                            format!("retry {}", codex_login::format_relative_reset(until, now))
                        })
                        .unwrap_or_else(|| "quota unavailable".into())
                } else if row.availability == "eligible" {
                    "available".into()
                } else {
                    row.availability.clone()
                },
                plan,
                email,
                percentage(row.rate_limits.primary.as_ref(), "-"),
                percentage(row.rate_limits.secondary.as_ref(), "-"),
                row.warmup.as_deref().unwrap_or("-").to_string(),
                label,
                quota_display::observed_at(
                    row.rate_limits
                        .primary_observed_at()
                        .map(|at| at.timestamp()),
                    now,
                ),
                quota_display::observed_at(
                    row.rate_limits
                        .secondary_observed_at()
                        .map(|at| at.timestamp()),
                    now,
                ),
                quota_display::reset(
                    row.rate_limits.primary.as_ref(),
                    quota_display::QuotaWindow::Primary,
                    now,
                ),
                quota_display::reset(
                    row.rate_limits.secondary.as_ref(),
                    quota_display::QuotaWindow::Secondary,
                    now,
                ),
            ]),
        }
        lines.push(
            cells
                .iter()
                .map(|cell| sanitize(cell))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
