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
    _view: AccountView,
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
        let labels = inventory
            .accounts
            .iter()
            .map(|row| {
                fit(
                    &mask_identity(row.label.as_deref().unwrap_or(&row.profile_id)),
                    /*width*/ 20,
                )
            })
            .collect::<Vec<_>>();
        // Masked, truncated, and duplicated labels cannot be used as exact selectors.
        let show_profile = options.show_profile
            || labels.iter().enumerate().any(|(index, label)| {
                label.is_empty()
                    || inventory.accounts[index].label.as_deref() != Some(label.as_str())
                    || labels[..index].contains(label)
            });
        let mut headers = vec!["ACCOUNT".into()];
        if show_profile {
            headers.push("PROFILE".into());
        }
        headers.extend(["NOW", "PLAN", "PRIORITY", "LOGIN", "POOL"].map(String::from));
        headers.push(window_heading(
            inventory
                .accounts
                .iter()
                .map(|row| row.rate_limits.primary.as_ref()),
            "PRIMARY",
        ));
        headers.push(window_heading(
            inventory
                .accounts
                .iter()
                .map(|row| row.rate_limits.secondary.as_ref()),
            "SECONDARY",
        ));
        headers.push("UPDATED".into());
        let rows = inventory
            .accounts
            .iter()
            .zip(&labels)
            .map(|(row, label)| {
                let mut cells = vec![label.clone()];
                if show_profile {
                    cells.push(if options.show_profile {
                        row.profile_id.clone()
                    } else {
                        short_id(&row.profile_id)
                    });
                }
                cells.extend([
                    if row.active { "*".into() } else { "-".into() },
                    fit(row.plan.as_deref().unwrap_or("unknown"), /*width*/ 12),
                    row.priority.to_string(),
                    row.login.label().into(),
                    match row.cooldown_until {
                        Some(until) if row.availability == "cooldown" => format!(
                            "retry {}",
                            duration(until.signed_duration_since(now).num_seconds())
                        ),
                        Some(_) | None => sanitize(&row.availability),
                    },
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
                    updated(row, now),
                ]);
                cells
            })
            .collect::<Vec<_>>();
        let widths = column_widths(&headers, &rows);
        if widths.iter().sum::<usize>() + 2 * (widths.len() - 1) <= columns {
            lines.push(aligned_row(&headers, &widths, &headers));
            for (cells, row) in rows.iter().zip(&inventory.accounts) {
                lines.push(aligned_row(cells, &widths, &headers));
                if options.details {
                    lines.extend(details(row, now));
                }
            }
        } else {
            for row in &inventory.accounts {
                lines.push(format!(
                    "{}{}",
                    mask_identity(row.label.as_deref().unwrap_or(&row.profile_id)),
                    if row.active { "  [current]" } else { "" },
                ));
                if show_profile {
                    lines.push(format!(
                        "  Profile: {}",
                        if options.show_profile {
                            row.profile_id.clone()
                        } else {
                            short_id(&row.profile_id)
                        }
                    ));
                }
                lines.push(format!("  Login: {}", row.login.label()));
                lines.push(format!("  Pool: {}", sanitize(&row.availability)));
                if let Some(until) = row.cooldown_until {
                    lines.push(format!(
                        "  Local retry: in {}",
                        duration(until.signed_duration_since(now).num_seconds())
                    ));
                }
                lines.push(format!(
                    "  Plan: {}; priority: {}",
                    row.plan.as_deref().unwrap_or("unknown"),
                    row.priority
                ));
                lines.push(format!(
                    "  {} used: {}",
                    window_label(
                        row.rate_limits
                            .primary
                            .as_ref()
                            .and_then(|window| window.window_minutes),
                        "Primary"
                    ),
                    quota_cell(
                        row.rate_limits.primary.as_ref(),
                        row.rate_limits.primary_observed_at(),
                        now
                    )
                ));
                lines.push(format!(
                    "  {} used: {}",
                    window_label(
                        row.rate_limits
                            .secondary
                            .as_ref()
                            .and_then(|window| window.window_minutes),
                        "Secondary"
                    ),
                    quota_cell(
                        row.rate_limits.secondary.as_ref(),
                        row.rate_limits.secondary_observed_at(),
                        now
                    )
                ));
                lines.push(format!("  Updated: {} (oldest sample)", updated(row, now)));
                if options.details {
                    lines.extend(details(row, now));
                }
                lines.push(String::new());
            }
        }
    }
    lines.push("Cached status; UPDATED = oldest quota sample; retry = local.".into());
    if options.details {
        lines.push("* current; priority: lower is preferred; login validity not checked.".into());
    } else {
        lines.push("* current; more: --details / --show-profile".into());
    }
    for warning in &inventory.warnings {
        lines.push(format!("Warning: {warning}"));
    }
    let wrap_options = textwrap::Options::new(columns)
        .word_separator(textwrap::WordSeparator::AsciiSpace)
        .word_splitter(textwrap::WordSplitter::NoHyphenation);
    lines
        .iter()
        .flat_map(|line| {
            let line = sanitize(line);
            textwrap::wrap(&line, &wrap_options)
                .into_iter()
                .map(std::borrow::Cow::into_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn details(row: &AccountRow, now: DateTime<Utc>) -> Vec<String> {
    let mut lines = vec![
        format!("  Profile: {}", row.profile_id),
        format!(
            "  Email: {}",
            mask_identity(row.email.as_deref().unwrap_or("unknown"))
        ),
    ];
    for (name, window, observed, kind) in [
        (
            "Primary",
            row.rate_limits.primary.as_ref(),
            row.rate_limits.primary_observed_at(),
            quota_display::QuotaWindow::Primary,
        ),
        (
            "Secondary",
            row.rate_limits.secondary.as_ref(),
            row.rate_limits.secondary_observed_at(),
            quota_display::QuotaWindow::Secondary,
        ),
    ] {
        lines.push(format!(
            "  {name} ({}):",
            window_label(
                window.and_then(|window| window.window_minutes),
                "duration unreported"
            )
        ));
        lines.push(format!(
            "    Last sample: {}",
            window
                .filter(|window| window.used_percent.is_finite() && window.used_percent >= 0.0)
                .map(|window| {
                    let used = window.used_percent;
                    format!("{used}% used")
                })
                .unwrap_or_else(|| "unknown".into())
        ));
        lines.push(format!(
            "    Updated: {}",
            quota_display::observed_at(observed.map(|at| at.timestamp()), now)
        ));
        lines.push(format!(
            "    Reset: {}",
            quota_display::reset(window, kind, now)
        ));
        if window.is_some_and(|window| window.resets_at.is_some_and(|at| at <= now)) {
            lines.push("    Current usage: unknown; reset passed, refresh needed.".into());
        } else if observed.is_some_and(|at| at > now) {
            lines.push("    Current usage: unknown; observation is future-dated.".into());
        }
    }
    lines.push(format!(
        "  Warmup: {}",
        row.warmup.as_deref().unwrap_or("unknown")
    ));
    lines
}

fn column_widths(headers: &[String], rows: &[Vec<String>]) -> Vec<usize> {
    headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| UnicodeWidthStr::width(row[index].as_str()))
                .fold(UnicodeWidthStr::width(header.as_str()), usize::max)
        })
        .collect()
}

fn aligned_row(cells: &[String], widths: &[usize], headers: &[String]) -> String {
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (cell, width))| {
            let padding = " ".repeat(width.saturating_sub(UnicodeWidthStr::width(cell.as_str())));
            if headers[index] == "PRIORITY" || headers[index].ends_with(" USED") {
                format!("{padding}{cell}")
            } else if index + 1 == cells.len() {
                cell.clone()
            } else {
                format!("{cell}{padding}")
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
    let Some(window) =
        window.filter(|window| window.used_percent.is_finite() && window.used_percent >= 0.0)
    else {
        return "unknown".into();
    };
    if window.resets_at.is_some_and(|at| at <= now) {
        return "stale".into();
    }
    if observed.is_some_and(|at| at > now) {
        return "unknown".into();
    }
    match window.used_percent {
        used if used > 0.0 && used < 1.0 => "<1%".into(),
        used if used > 99.0 && used < 100.0 => ">99%".into(),
        used => format!("{used:.0}%"),
    }
}

fn window_heading<'a>(
    windows: impl Iterator<Item = Option<&'a AccountRateLimitWindow>>,
    fallback: &str,
) -> String {
    let mut windows = windows.flatten().map(|window| window.window_minutes);
    if let Some(Some(minutes)) = windows.next()
        && minutes > 0
        && windows.all(|value| value == Some(minutes))
    {
        format!(
            "{} USED",
            window_label(Some(minutes), fallback).to_uppercase()
        )
    } else {
        format!("{fallback} USED")
    }
}

fn window_label(minutes: Option<i64>, fallback: &str) -> String {
    match minutes {
        Some(10080) => "week".into(),
        Some(minutes) if minutes > 0 && minutes % 1440 == 0 => format!("{}d", minutes / 1440),
        Some(minutes) if minutes > 0 && minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) if minutes > 0 => format!("{minutes}m"),
        Some(_) | None => fallback.into(),
    }
}

fn updated(row: &AccountRow, now: DateTime<Utc>) -> String {
    let times = [
        (
            row.rate_limits.primary.as_ref(),
            row.rate_limits.primary_observed_at(),
        ),
        (
            row.rate_limits.secondary.as_ref(),
            row.rate_limits.secondary_observed_at(),
        ),
    ]
    .into_iter()
    .filter(|(window, _)| window.is_some())
    .map(|(_, at)| at)
    .collect::<Vec<_>>();
    if times.iter().any(|at| at.is_some_and(|at| at > now)) {
        return "clock skew".into();
    }
    if times.iter().any(Option::is_none) {
        return "unknown".into();
    }
    match times.into_iter().flatten().min() {
        Some(at) => {
            let minutes = now.signed_duration_since(at).num_minutes();
            match minutes {
                0 => "<1m ago".into(),
                1..=59 => format!("{minutes}m ago"),
                60..=1439 => format!("{}h ago", minutes / 60),
                _ => format!("{}d ago", minutes / 1440),
            }
        }
        None => "unknown".into(),
    }
}

fn duration(seconds: i64) -> String {
    let minutes = u64::try_from(seconds)
        .unwrap_or_default()
        .div_ceil(/*rhs*/ 60);
    match minutes {
        0..=59 => format!("{minutes}m"),
        60..=1439 => format!("{}h{}m", minutes / 60, minutes % 60),
        _ => format!(
            "{}d{}h{}m",
            minutes / 1440,
            minutes % 1440 / 60,
            minutes % 60
        ),
    }
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
