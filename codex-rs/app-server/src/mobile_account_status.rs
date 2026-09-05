//! Compact, bounded labels for the existing mobile status UI.

use codex_app_server_protocol::AccountPoolAvailability;
use codex_app_server_protocol::AccountPoolReadResponse;
use codex_app_server_protocol::RateLimitSnapshot;

pub(crate) fn pool_caption(pool: &AccountPoolReadResponse) -> Option<String> {
    pool.enabled.then(|| {
        let ready = pool
            .accounts
            .iter()
            .filter(|account| matches!(account.availability, AccountPoolAvailability::Available))
            .count();
        format!("Codex · {ready}/{} ready", pool.accounts.len())
    })
}

pub(crate) fn account_caption(pool: &AccountPoolReadResponse) -> String {
    let active = pool.accounts.iter().find(|account| account.is_active);
    let label = active
        .and_then(|account| account.label.as_deref().or(account.email.as_deref()))
        .or(pool.active_profile_id.as_deref())
        .unwrap_or("No active account");
    let label = compact_label(label, 32);
    match pool_caption(pool) {
        Some(pool) => format!("{label} · {pool}"),
        None => "Account pool is not configured.".to_string(),
    }
}

pub(crate) fn compact_label(text: &str, max: usize) -> String {
    let mut chars = text.chars().filter(|character| !character.is_control());
    let mut label: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        label.push('…');
    }
    label
}

pub(crate) fn overlay_snapshot(snapshot: &mut RateLimitSnapshot, caption: &str) {
    if snapshot.limit_id.as_deref().is_none_or(|id| id == "codex") {
        snapshot.limit_name = Some(caption.to_string());
    }
}
