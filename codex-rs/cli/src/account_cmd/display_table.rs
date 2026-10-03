use chrono::DateTime;
use chrono::Utc;
use codex_login::AccountRateLimitWindow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::display::AccountInventory;
use super::display::AccountOutputOptions;
use super::display::AccountRow;
use super::display::AccountView;
use super::quota_display;

pub(super) fn render_table(
    inventory: &AccountInventory,
    view: AccountView,
    options: AccountOutputOptions,
    columns: usize,
) -> String {
    let mut lines = Vec::new();
    let now = inventory.now();
    if inventory.suspended {
        lines.push("Account pool paused; enrolled profiles are retained.".into());
    }
    if let Some(settings) = &inventory.settings {
        lines.push(format!(
            "Rotation: {}; return to preferred: {}; early switch: {}",
            settings.rotation_strategy, settings.return_to_preferred, settings.preemptive_switch
        ));
    }
    if inventory.accounts.is_empty() {
        lines.push("No account profiles. Add one with `codex account add`.".into());
    } else {
        let full_headers = [
            "ACCOUNT",
            "PROFILE",
            "NOW",
            "LOGIN",
            "AVAILABILITY",
            "PLAN",
            "PRI",
            "5H USED",
            "WEEK USED",
        ];
        let mut rows = inventory
            .accounts
            .iter()
            .map(|row| {
                vec![
                    fit(
                        &mask_identity(row.label.as_deref().unwrap_or(&row.profile_id)),
                        /*width*/ 20,
                    ),
                    if options.show_profile {
                        row.profile_id.clone()
                    } else {
                        short_id(&row.profile_id)
                    },
                    if row.active { "*".into() } else { "-".into() },
                    row.login.label().into(),
                    sanitize(&row.availability),
                    fit(row.plan.as_deref().unwrap_or("unknown"), /*width*/ 12),
                    row.priority.to_string(),
                    quota_cell(
                        row.rate_limits.primary.as_ref(),
                        row.rate_limits.primary_observed_at(),
                        now,
                    ),
                    quota_cell(
                        row.rate_limits.secondary.as_ref(),
                        row.rate_limits.secondary_observed_at(),
                        now,
                    ),
                ]
            })
            .collect::<Vec<_>>();
        let mut headers = full_headers.to_vec();
        let mut widths = column_widths(&headers, &rows);
        if widths.iter().sum::<usize>() + 2 * (widths.len() - 1) > columns && !options.show_profile
        {
            for index in [5, 1] {
                headers.remove(index);
                for row in &mut rows {
                    row.remove(index);
                }
            }
            widths = column_widths(&headers, &rows);
        }
        if widths.iter().sum::<usize>() + 2 * (widths.len() - 1) <= columns {
            lines.push(aligned_row(
                &headers
                    .iter()
                    .map(|header| (*header).into())
                    .collect::<Vec<_>>(),
                &widths,
            ));
            for (cells, row) in rows.iter().zip(&inventory.accounts) {
                lines.push(aligned_row(cells, &widths));
                if let Some(until) = row.cooldown_until {
                    lines.push(format!(
                        "  Cooldown: {}",
                        codex_login::format_relative_reset(until, now)
                    ));
                }
                if options.details {
                    lines.extend(details(row, now));
                }
            }
        } else {
            for row in &inventory.accounts {
                lines.push(format!(
                    "{}  [{} {}]",
                    mask_identity(row.label.as_deref().unwrap_or(&row.profile_id)),
                    if row.active { "*" } else { "-" },
                    if options.show_profile {
                        row.profile_id.clone()
                    } else {
                        short_id(&row.profile_id)
                    }
                ));
                lines.push(format!("  Login: {}", row.login.label()));
                lines.push(format!("  Availability: {}", sanitize(&row.availability)));
                if let Some(until) = row.cooldown_until {
                    lines.push(format!(
                        "  Cooldown: {}",
                        codex_login::format_relative_reset(until, now)
                    ));
                }
                lines.push(format!(
                    "  Plan: {}; priority: {}",
                    row.plan.as_deref().unwrap_or("unknown"),
                    row.priority
                ));
                lines.push(format!(
                    "  5h used: {}",
                    quota_cell(
                        row.rate_limits.primary.as_ref(),
                        row.rate_limits.primary_observed_at(),
                        now
                    )
                ));
                lines.push(format!(
                    "  Week used: {}",
                    quota_cell(
                        row.rate_limits.secondary.as_ref(),
                        row.rate_limits.secondary_observed_at(),
                        now
                    )
                ));
                if options.details {
                    lines.extend(details(row, now));
                }
            }
        }
    }
    if matches!(view, AccountView::Pool) {
        lines.push("Quota: cached usage (age per window).".into());
    }
    lines.push("Login: stored credentials, unverified.".into());
    lines.push("Availability: cached scheduling state.".into());
    lines.push("* current; use unique label or full ID.".into());
    if !options.details {
        lines.push("IDs: --show-profile; details: --details.".into());
    }
    lines.push("Full metadata: --format json.".into());
    for warning in &inventory.warnings {
        lines.push(format!("Warning: {warning}"));
    }
    lines
        .iter()
        .map(|line| fit(line, columns))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn details(row: &AccountRow, now: DateTime<Utc>) -> Vec<String> {
    vec![
        format!("  Profile: {}", row.profile_id),
        format!(
            "  Email: {}",
            mask_identity(row.email.as_deref().unwrap_or("unknown"))
        ),
        format!(
            "  Primary observed: {}",
            quota_display::observed_at(
                row.rate_limits
                    .primary_observed_at()
                    .map(|at| at.timestamp()),
                now
            )
        ),
        format!(
            "  Primary reset: {}",
            quota_display::reset(
                row.rate_limits.primary.as_ref(),
                quota_display::QuotaWindow::Primary,
                now
            )
        ),
        format!(
            "  Secondary observed: {}",
            quota_display::observed_at(
                row.rate_limits
                    .secondary_observed_at()
                    .map(|at| at.timestamp()),
                now
            )
        ),
        format!(
            "  Secondary reset: {}",
            quota_display::reset(
                row.rate_limits.secondary.as_ref(),
                quota_display::QuotaWindow::Secondary,
                now
            )
        ),
        format!("  Warmup: {}", row.warmup.as_deref().unwrap_or("unknown")),
    ]
}

fn column_widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| UnicodeWidthStr::width(row[index].as_str()))
                .fold(UnicodeWidthStr::width(*header), usize::max)
        })
        .collect()
}

fn aligned_row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (cell, width))| {
            if index + 1 == cells.len() {
                cell.clone()
            } else {
                format!(
                    "{cell}{}",
                    " ".repeat(width.saturating_sub(UnicodeWidthStr::width(cell.as_str())))
                )
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

pub(super) fn percentage(window: Option<&AccountRateLimitWindow>, unknown: &str) -> String {
    window
        .filter(|window| window.used_percent.is_finite() && window.used_percent >= 0.0)
        .map(|window| format!("{:.0}", window.used_percent))
        .unwrap_or_else(|| unknown.into())
}

fn quota_cell(
    window: Option<&AccountRateLimitWindow>,
    observed: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    let used = percentage(window, "unknown");
    if used == "unknown" {
        return used;
    }
    let age = match observed {
        Some(at) if at.timestamp() > now.timestamp() => "clock skew".into(),
        Some(at) => {
            let minutes = now.signed_duration_since(at).num_minutes();
            match minutes {
                0..=59 => format!("{minutes}m"),
                60..=1439 => format!("{}h", minutes / 60),
                _ => format!("{}d", minutes / 1440),
            }
        }
        None => "age unknown".into(),
    };
    format!("{used}% ({age})")
}

fn short_id(id: &str) -> String {
    if id.len() <= 14 {
        id.to_string()
    } else {
        let suffix = id
            .chars()
            .rev()
            .take(/*n*/ 7)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("acct-…{suffix}")
    }
}

fn mask_identity(value: &str) -> String {
    sanitize(value)
        .split_whitespace()
        .map(|word| {
            if let Some((local, domain)) = word.split_once('@') {
                format!("{}***@{domain}", local.chars().next().unwrap_or('*'))
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn sanitize(value: &str) -> String {
    value.chars().map(|ch| {
        if ch.is_control() || matches!(ch, '\u{061c}' | '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}' | '\u{2066}'..='\u{2069}' | '\u{feff}') {
            ' '
        } else {
            ch
        }
    }).collect()
}

fn fit(value: &str, width: usize) -> String {
    let value = sanitize(value);
    if UnicodeWidthStr::width(value.as_str()) <= width {
        return value;
    }
    let mut result = String::new();
    for grapheme in value.graphemes(true) {
        if UnicodeWidthStr::width(result.as_str()) + UnicodeWidthStr::width(grapheme) + 1 > width {
            break;
        }
        result.push_str(grapheme);
    }
    result.push('…');
    result
}
