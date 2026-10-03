//! Passive, model-free reminders for expiring account-pool reset credits.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use chrono::Utc;
use codex_backend_client::AccountCreditExpiryReader;
use codex_backend_client::CreditExpirySnapshot;
use codex_login::AccountAvailability;
use codex_login::AccountPool;
use codex_login::AccountRuntimeStateStore;
use codex_login::CreditExpiryCandidate;
use codex_login::CreditExpiryReminderStore;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::WarningEvent;
use tokio_util::sync::CancellationToken;

use crate::execution_auth::ExecutionAuth;
use crate::execution_auth::ExecutionAuthMode;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

const MAX_CLAIM_ATTEMPTS: usize = 16;

/// One read-only metadata pass for the shared execution coordinator. A short
/// completed turn may fill the cache, but its presentation guard forbids late
/// warnings. Stopping the parent turn also cancels its HTTP reads.
pub(crate) struct AccountCreditExpiryReminders {
    home: PathBuf,
    reader: AccountCreditExpiryReader,
    store: CreditExpiryReminderStore,
    running: AtomicBool,
    cursor: AtomicUsize,
}

impl AccountCreditExpiryReminders {
    fn new(home: PathBuf) -> Self {
        Self {
            reader: AccountCreditExpiryReader::new(home.clone()),
            store: CreditExpiryReminderStore::new(home.clone()),
            home,
            running: AtomicBool::new(false),
            cursor: AtomicUsize::new(0),
        }
    }

    fn claim_warning(
        &self,
        execution_auth: &ExecutionAuth,
        pool: &Arc<AccountPool>,
        snapshots: &[CreditExpirySnapshot],
        presentation: &CancellationToken,
    ) -> Option<WarningEvent> {
        let now = Utc::now().timestamp();
        // Observe extensions without presenting a notice. An extension beyond 24 hours
        // must retain the original voucher budget rather than age out its old record.
        let runtime_store = AccountRuntimeStateStore::new(self.home.clone());
        let mut extensions_observed = 0;
        for snapshot in snapshots.iter().filter(|snapshot| snapshot.fresh) {
            if presentation.is_cancelled() {
                return None;
            }
            if !owner_is_current(pool, snapshot, &runtime_store) {
                continue;
            }
            for credit in snapshot
                .credits
                .iter()
                .filter(|credit| credit.expires_at.saturating_sub(now) > 24 * 3_600)
            {
                if extensions_observed >= MAX_CLAIM_ATTEMPTS {
                    break;
                }
                extensions_observed += 1;
                let _ = self.store.try_claim(
                    &CreditExpiryCandidate {
                        account_id: &snapshot.account_id,
                        chatgpt_user_id: &snapshot.chatgpt_user_id,
                        credit_id: &credit.id,
                        reset_type: "codex_rate_limits",
                        status: "available",
                        expires_at: credit.expires_at,
                    },
                    now,
                );
            }
        }
        let mut identities = HashSet::new();
        let mut candidates = Vec::new();
        for snapshot in snapshots.iter().filter(|snapshot| snapshot.fresh) {
            for credit in &snapshot.credits {
                if credit.expires_at > now
                    && credit.expires_at.saturating_sub(now) <= 24 * 3_600
                    && identities.insert((
                        snapshot.account_id.as_str(),
                        snapshot.chatgpt_user_id.as_str(),
                        credit.id.as_str(),
                    ))
                {
                    candidates.push((snapshot, credit));
                }
            }
        }
        candidates.sort_by_key(|(_, credit)| credit.expires_at);
        let count = candidates.len();
        if count == 0 {
            return None;
        }
        let start = self.cursor.load(Ordering::Relaxed) % count;
        // A large inventory must not cause hundreds of reminder-file reads on
        // every turn. Rotate a bounded batch so previously notified vouchers
        // cannot indefinitely hide other seats' notices.
        for offset in 0..count.min(MAX_CLAIM_ATTEMPTS) {
            let index = (start + offset) % count;
            self.cursor.store((index + 1) % count, Ordering::Relaxed);
            let (snapshot, credit) = candidates[index];
            if presentation.is_cancelled()
                || !execution_auth
                    .account_pool()
                    .is_some_and(|current| Arc::ptr_eq(&current, pool))
            {
                return None;
            }
            if !owner_is_current(pool, snapshot, &runtime_store) {
                continue;
            }
            let candidate = CreditExpiryCandidate {
                account_id: &snapshot.account_id,
                chatgpt_user_id: &snapshot.chatgpt_user_id,
                credit_id: &credit.id,
                reset_type: "codex_rate_limits",
                status: "available",
                expires_at: credit.expires_at,
            };
            if presentation.is_cancelled() {
                return None;
            }
            let reminder = self.store.try_claim(&candidate, now).ok()?;
            if reminder.is_none() {
                continue;
            }
            // A concurrent relogin/removal can invalidate a presentation even
            // after the durable reservation. Never refund that reservation.
            if presentation.is_cancelled()
                || !owner_is_current(pool, snapshot, &runtime_store)
                || !execution_auth
                    .account_pool()
                    .is_some_and(|current| Arc::ptr_eq(&current, pool))
            {
                return None;
            }
            let minutes = credit
                .expires_at
                .saturating_sub(Utc::now().timestamp())
                .saturating_add(59)
                / 60;
            if minutes <= 0 {
                return None;
            }
            let remaining = if minutes < 60 {
                format!("{minutes}m")
            } else {
                format!("{}h {:02}m", minutes / 60, minutes % 60)
            };
            let title = credit
                .title
                .as_deref()
                .map(|title| {
                    let title: String = title.chars().take(64).collect();
                    format!(" ({title})")
                })
                .unwrap_or_default();
            let others = if count > 1 {
                format!(
                    " {} other expiring credit(s) are available in the pool.",
                    count - 1
                )
            } else {
                String::new()
            };
            return Some(WarningEvent {
                message: format!(
                    "Codex reset credit{title} for account `{}` expires in {remaining}.{others} Review credits on the host with `codex account manage`. In mobile Remote, `/account manage` shows the account overview. A reset clears existing usage windows; it does not add quota. No credit was used.",
                    snapshot.label,
                ),
            });
        }
        None
    }
}

fn owner_is_current(
    pool: &AccountPool,
    snapshot: &CreditExpirySnapshot,
    runtime_store: &AccountRuntimeStateStore,
) -> bool {
    if !pool.snapshots().iter().any(|current| {
        current.profile.id == snapshot.profile_id
            && !current.profile.disabled
            && !matches!(
                current.availability,
                AccountAvailability::Disabled
                    | AccountAvailability::AuthenticationUnavailable { .. }
            )
    }) {
        return false;
    }
    let Some((_, manager)) = pool
        .auth_managers()
        .into_iter()
        .find(|(profile, _)| profile == &snapshot.profile_id)
    else {
        return false;
    };
    let revision = *manager.auth_change_state_receiver().borrow();
    let Some(auth) = manager.auth_cached() else {
        return false;
    };
    auth.is_chatgpt_auth()
        && auth.get_account_id().as_deref() == Some(snapshot.account_id.as_str())
        && auth.get_chatgpt_user_id().as_deref() == Some(snapshot.chatgpt_user_id.as_str())
        && runtime_store
            .validate_profile_auth(pool, &snapshot.profile_id, &auth)
            .unwrap_or(false)
        && *manager.auth_change_state_receiver().borrow() == revision
}

struct CreditExpiryTurnObserved;

pub(crate) struct CreditExpiryTurnGuard(CancellationToken);

impl Drop for CreditExpiryTurnGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct MetadataPassGuard(Arc<AccountCreditExpiryReminders>);

impl Drop for MetadataPassGuard {
    fn drop(&mut self) {
        self.0.running.store(/*val*/ false, Ordering::Release);
    }
}

/// Called only after prompt hooks accepted and recorded the ordinary user
/// input. This spawns at most one shared worker and adds no prompt history.
pub(crate) fn spawn_turn_reminder(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    execution_auth: Arc<ExecutionAuth>,
    mode: &ExecutionAuthMode,
    input: &[TurnInput],
    cancellation: &CancellationToken,
) -> Option<CreditExpiryTurnGuard> {
    let root = match &turn.session_source {
        SessionSource::Cli
        | SessionSource::VSCode
        | SessionSource::Exec
        | SessionSource::Mcp
        | SessionSource::Custom(_)
        | SessionSource::Unknown => turn.parent_thread_id.is_none(),
        SessionSource::Internal(_) | SessionSource::SubAgent(_) => false,
    };
    if !root || !mode.is_pooled() || cancellation.is_cancelled()
        || !input.iter().any(|input| matches!(input, TurnInput::UserInput { content, metadata, .. } if !content.is_empty() && metadata.origin.is_user()))
        || !turn.extension_data.insert_if(CreditExpiryTurnObserved, |previous| previous.is_none())
    {
        return None;
    }
    let pool = execution_auth.account_pool()?;
    let coordinator = Arc::clone(execution_auth.credit_expiry_reminders.get_or_init(|| {
        Arc::new(AccountCreditExpiryReminders::new(
            turn.config.codex_home.to_path_buf(),
        ))
    }));
    if coordinator.home.as_path() != turn.config.codex_home.as_path()
        || coordinator
            .running
            .compare_exchange(
                /*current*/ false,
                /*new*/ true,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        return None;
    }
    let pass_guard = MetadataPassGuard(Arc::clone(&coordinator));
    let presentation = cancellation.child_token();
    let turn_guard = CreditExpiryTurnGuard(presentation.clone());
    let cancellation = cancellation.clone();
    tokio::spawn(async move {
        let _pass_guard = pass_guard;
        let snapshots = coordinator
            .reader
            .collect_due(&pool, &turn.config.chatgpt_base_url, &cancellation)
            .await;
        if let Some(warning) =
            coordinator.claim_warning(execution_auth.as_ref(), &pool, &snapshots, &presentation)
        {
            tokio::select! {
                biased;
                _ = presentation.cancelled() => {}
                _ = session.send_event(&turn, EventMsg::Warning(warning)) => {}
            }
        }
    });
    Some(turn_guard)
}

#[cfg(test)]
#[path = "account_credit_expiry_reminders_tests.rs"]
mod tests;
