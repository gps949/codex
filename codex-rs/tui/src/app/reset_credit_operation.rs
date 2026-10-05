use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;

use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::PendingAccountRateLimitResetCredit;
use codex_login::AccountRuntimeStateStore;
use serde::Deserialize;
use serde::Serialize;

use super::App;

const RESET_CREDIT_JOURNAL_FILE: &str = "tui-reset-credit-journal.json";
const MAX_RESET_CREDIT_JOURNAL_BYTES: usize = 128 * 1024;
const MAX_PENDING_RESET_CREDITS: usize = 64;

/// Immutable authorization and replay identity; never contains account credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResetCreditOperation {
    pub(crate) owner_key: String,
    pub(crate) idempotency_key: String,
    pub(crate) credit_id: Option<String>,
}

impl ResetCreditOperation {
    fn validate(&self) -> io::Result<()> {
        if !valid_reset_owner_key(&self.owner_key)
            || self.idempotency_key.is_empty()
            || self.idempotency_key.len() > 128
            || self
                .credit_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 256)
        {
            return Err(io::Error::other("Invalid pending reset identity"));
        }
        Ok(())
    }
}

impl From<PendingAccountRateLimitResetCredit> for ResetCreditOperation {
    fn from(pending: PendingAccountRateLimitResetCredit) -> Self {
        Self {
            owner_key: pending.owner_key,
            idempotency_key: pending.idempotency_key,
            credit_id: pending.credit_id,
        }
    }
}

pub(crate) fn valid_reset_owner_key(owner: &str) -> bool {
    owner.len() == 64
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResetCreditJournal {
    version: u8,
    pending: Vec<ResetCreditOperation>,
}

impl ResetCreditJournal {
    fn read_unlocked(home: &Path) -> io::Result<Vec<ResetCreditOperation>> {
        let file = match std::fs::File::open(home.join(RESET_CREDIT_JOURNAL_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take((MAX_RESET_CREDIT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESET_CREDIT_JOURNAL_BYTES {
            return Err(io::Error::other("Pending reset journal is too large"));
        }
        let journal: Self = serde_json::from_slice(&bytes)?;
        if journal.version != 1 || journal.pending.len() > MAX_PENDING_RESET_CREDITS {
            return Err(io::Error::other("Unsupported pending reset journal"));
        }
        for (index, operation) in journal.pending.iter().enumerate() {
            operation.validate()?;
            if journal.pending[..index].iter().any(|previous| {
                previous.owner_key == operation.owner_key
                    || previous.idempotency_key == operation.idempotency_key
            }) {
                return Err(io::Error::other("Conflicting pending reset identities"));
            }
        }
        Ok(journal.pending)
    }

    fn read(home: &Path) -> io::Result<Vec<ResetCreditOperation>> {
        let _lock = Self::lock(home)?;
        Self::read_unlocked(home)
    }

    fn lock(home: &Path) -> io::Result<std::fs::File> {
        AccountRuntimeStateStore::new(home.to_path_buf())
            .try_lock_reset_credit()?
            .ok_or_else(|| io::Error::other("A reset operation is busy. Please try again."))
    }

    fn write(home: &Path, pending: &[ResetCreditOperation]) -> io::Result<()> {
        let bytes = serde_json::to_vec(&Self {
            version: 1,
            pending: pending.to_vec(),
        })?;
        if bytes.len() > MAX_RESET_CREDIT_JOURNAL_BYTES {
            return Err(io::Error::other("Pending reset journal is full"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(home)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(home.join(RESET_CREDIT_JOURNAL_FILE))
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
        Ok(())
    }

    fn remember(
        home: &Path,
        operation: &ResetCreditOperation,
    ) -> io::Result<Vec<ResetCreditOperation>> {
        operation.validate()?;
        let _lock = Self::lock(home)?;
        let mut pending = Self::read_unlocked(home)?;
        if let Some(original) = pending.iter().find(|original| {
            original.owner_key == operation.owner_key
                || original.idempotency_key == operation.idempotency_key
        }) {
            if original != operation {
                return Err(io::Error::other(
                    "Review the original pending reset before using another",
                ));
            }
            return Ok(pending);
        }
        if pending.len() >= MAX_PENDING_RESET_CREDITS {
            return Err(io::Error::other("Pending reset journal is full"));
        }
        pending.push(operation.clone());
        Self::write(home, &pending)?;
        Ok(pending)
    }

    fn settle(
        home: &Path,
        operation: &ResetCreditOperation,
    ) -> io::Result<Vec<ResetCreditOperation>> {
        let _lock = Self::lock(home)?;
        let mut pending = Self::read_unlocked(home)?;
        if let Some(index) = pending.iter().position(|pending| pending == operation) {
            pending.remove(index);
            Self::write(home, &pending)?;
        } else if pending.iter().any(|pending| {
            pending.owner_key == operation.owner_key
                || pending.idempotency_key == operation.idempotency_key
        }) {
            return Err(io::Error::other("Pending reset identity changed"));
        }
        Ok(pending)
    }
}

#[derive(Default)]
pub(super) struct ResetCreditOperationState {
    pending: Vec<ResetCreditOperation>,
    in_flight: Option<(u64, ResetCreditOperation)>,
    pub(super) post_consume: Option<(u64, ResetCreditOperation)>,
    persistence_error: Option<String>,
}

impl App {
    pub(super) fn restore_reset_credit_operations(&mut self) {
        match ResetCreditJournal::read(self.local_settings.codex_home.as_path()) {
            Ok(pending) => {
                self.reset_credit_operations.pending = pending;
                self.reset_credit_operations.persistence_error = None;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to load TUI pending reset journal");
                self.reset_credit_operations.persistence_error = Some(
                    "Couldn't read pending resets. Check your Codex home permissions or upgrade Codex, then try again.".to_string(),
                );
            }
        }
        self.sync_reset_credit_operation();
    }

    pub(super) fn sync_reset_credit_operation(&mut self) {
        self.chat_widget.set_pending_reset_credit_operation(
            self.reset_credit_operations
                .pending
                .iter()
                .find(|operation| self.chat_widget.reset_credit_owner_matches(operation))
                .or_else(|| self.reset_credit_operations.pending.first())
                .cloned(),
            self.reset_credit_operations.persistence_error.clone(),
        );
    }

    pub(super) fn observe_reset_credit_read(&mut self, response: &GetAccountRateLimitsResponse) {
        self.chat_widget
            .set_reset_credit_owner(response.reset_owner_key.clone());
        self.restore_reset_credit_operations();
        if let Some(pending) = response.pending_reset_credit.clone() {
            let operation = ResetCreditOperation::from(pending);
            if response.reset_owner_key.as_deref() != Some(operation.owner_key.as_str()) {
                return;
            }
            if self
                .reset_credit_operations
                .pending
                .iter()
                .any(|original| original.owner_key == operation.owner_key)
            {
                return;
            }
            match ResetCreditJournal::remember(self.local_settings.codex_home.as_path(), &operation)
            {
                Ok(pending) => self.reset_credit_operations.pending = pending,
                Err(error) => {
                    tracing::warn!(%error, "failed to preserve server pending reset");
                    if self.reset_credit_operations.pending.is_empty() {
                        self.reset_credit_operations.pending.push(operation);
                    }
                    self.reset_credit_operations.persistence_error = Some(
                        "Couldn't preserve the pending reset. Fix Codex home permissions and try again.".to_string(),
                    );
                }
            }
            self.sync_reset_credit_operation();
        }
    }

    pub(super) fn prepare_reset_credit_operation(
        &mut self,
        operation: &ResetCreditOperation,
    ) -> Option<u64> {
        if self.reset_credit_operations.in_flight.is_some()
            || !self.chat_widget.reset_credit_operation_can_start(operation)
        {
            return None;
        }
        // The short journal transaction releases the server's spending lock before any RPC.
        match ResetCreditJournal::remember(self.local_settings.codex_home.as_path(), operation) {
            Ok(pending) => {
                self.reset_credit_operations.pending = pending;
                self.reset_credit_operations.persistence_error = None;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to save TUI reset before dispatch");
                self.reset_credit_operations.persistence_error = Some(
                    "Couldn't save this reset. No request was sent. Check Codex home permissions and try again.".to_string(),
                );
                self.sync_reset_credit_operation();
                self.chat_widget.show_reset_credit_persistence_error();
                return None;
            }
        }
        self.sync_reset_credit_operation();
        let request_id = self
            .chat_widget
            .start_rate_limit_reset_consumption(operation)?;
        self.reset_credit_operations.in_flight = Some((request_id, operation.clone()));
        Some(request_id)
    }

    pub(super) fn finish_reset_credit_operation(
        &mut self,
        request_id: u64,
        operation: ResetCreditOperation,
        mut result: Result<ConsumeAccountRateLimitResetCreditResponse, String>,
    ) -> bool {
        if self.reset_credit_operations.in_flight.as_ref() != Some(&(request_id, operation.clone()))
        {
            return false;
        }
        self.reset_credit_operations.in_flight = None;
        if result.is_ok() {
            match ResetCreditJournal::settle(self.local_settings.codex_home.as_path(), &operation) {
                Ok(pending) => {
                    self.reset_credit_operations.pending = pending;
                    self.reset_credit_operations.persistence_error = None;
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to settle TUI pending reset");
                    result = Err("Couldn't save the reset receipt. Review the original reset before trying another.".to_string());
                }
            }
        }
        let refresh_quota =
            self.chat_widget
                .finish_rate_limit_reset_consume(request_id, operation.clone(), result);
        self.sync_reset_credit_operation();
        if !refresh_quota {
            return false;
        }
        self.reset_credit_operations.post_consume = Some((request_id, operation));
        self.rate_limit_hard_stop_generation = self.rate_limit_hard_stop_generation.wrapping_add(1);
        self.rate_limit_refresh_state.invalidate_recovery();
        // A replayed receipt proves the operation completed, not that current quota recovered.
        // The same-owner usage read must establish permission before clearing a current banner.
        true
    }

    pub(super) fn reset_credit_refresh_matches(
        &self,
        request_id: u64,
        response: &GetAccountRateLimitsResponse,
    ) -> bool {
        self.reset_credit_operations
            .post_consume
            .as_ref()
            .is_some_and(|(id, operation)| {
                *id == request_id
                    && response.reset_owner_key.as_deref() == Some(operation.owner_key.as_str())
                    && self.chat_widget.reset_credit_owner_matches(operation)
            })
    }

    pub(super) fn reset_credit_refresh_error_matches(&self, request_id: u64) -> bool {
        self.reset_credit_operations
            .post_consume
            .as_ref()
            .is_some_and(|(id, operation)| {
                *id == request_id && self.chat_widget.reset_credit_owner_matches(operation)
            })
    }
}

#[cfg(test)]
#[path = "reset_credit_operation_tests.rs"]
mod tests;
