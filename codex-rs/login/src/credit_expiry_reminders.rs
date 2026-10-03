//! Bounded, restart-safe notices for expiring Codex reset vouchers. This policy
//! stores hashed identity metadata only and never redeems a voucher.

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

const STATE_FILE: &str = ".credit-expiry-reminders.json";
const MAX_RECORDS: usize = 1_024;
const MAX_STATE_BYTES: u64 = 512 * 1_024;
const RETENTION_GRACE_SECONDS: i64 = 30 * 24 * 3_600;
const BANDS: [u8; 6] = [24, 18, 12, 6, 3, 1];

/// An authenticated voucher observation. Profile labels are deliberately absent:
/// duplicate profiles for the same account and ChatGPT user share a notice budget.
#[derive(Clone, Copy)]
pub struct CreditExpiryCandidate<'a> {
    pub account_id: &'a str,
    pub chatgpt_user_id: &'a str,
    pub credit_id: &'a str,
    pub reset_type: &'a str,
    pub status: &'a str,
    pub expires_at: i64,
}

/// A durable presentation reservation. Dropping it does not release its budget;
/// a crash or unanswered question therefore cannot endlessly repeat a notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditExpiryReminder {
    key: String,
    pub band_hours: u8,
    pub notice_number: u8,
    pub expires_at: i64,
}

/// These responses control reminders only; neither grants permission to spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CreditExpiryReminderResponse {
    SnoozeUntilNextStage,
    MuteVoucher,
}

/// Persists operational reminder state under the account metadata transaction
/// lock. No returned object retains a file lock across a question or network call.
pub struct CreditExpiryReminderStore {
    home: PathBuf,
}

impl CreditExpiryReminderStore {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// Reserves the current band before presentation. Missed bands are skipped,
    /// including first detection late in a voucher's lifetime. A busy metadata
    /// lock or a full live history yields no reservation, without waiting for
    /// another transaction. Feed later-expiring observations too: they preserve
    /// existing budgets across extensions. Expired records have a 30-day grace.
    pub fn try_claim(
        &self,
        candidate: &CreditExpiryCandidate<'_>,
        now: i64,
    ) -> io::Result<Option<CreditExpiryReminder>> {
        if candidate.reset_type != "codex_rate_limits"
            || candidate.status != "available"
            || now <= 0
            || chrono::DateTime::from_timestamp(now, /*nsecs*/ 0).is_none()
            || chrono::DateTime::from_timestamp(candidate.expires_at, /*nsecs*/ 0).is_none()
            || candidate.expires_at <= now
            || [candidate.account_id, candidate.chatgpt_user_id]
                .iter()
                .any(|id| id.trim().is_empty() || id.len() > 512)
            || candidate.credit_id.trim().is_empty()
            || candidate.credit_id.len() > 256
        {
            return Ok(None);
        }
        let remaining = candidate.expires_at.saturating_sub(now);
        let band_hours = BANDS
            .into_iter()
            .rev()
            .find(|hours| remaining <= i64::from(*hours) * 3_600);
        let mut digest = Sha256::new();
        digest.update(b"codex-credit-expiry-reminder-v1");
        for id in [
            candidate.account_id,
            candidate.chatgpt_user_id,
            candidate.credit_id,
        ] {
            digest.update((id.len() as u64).to_le_bytes());
            digest.update(id.as_bytes());
        }
        let key = format!("{:x}", digest.finalize());
        let Some(_lock) = crate::account_file::try_lock(&self.home)? else {
            return Ok(None);
        };
        let mut state = self.read()?;
        if now < state.last_observed_at {
            return Ok(None);
        }
        state.last_observed_at = now;
        let mut changed = false;
        if let Some(record) = state.records.get_mut(&key)
            && candidate.expires_at > record.expires_at
        {
            record.expires_at = candidate.expires_at;
            changed = true;
        }
        let before = state.records.len();
        state
            .records
            .retain(|_, record| record.expires_at.saturating_add(RETENTION_GRACE_SECONDS) >= now);
        changed |= before != state.records.len();
        let Some(band_hours) = band_hours else {
            if changed {
                self.write(&state)?;
            }
            return Ok(None);
        };
        let notice_number = if let Some(record) = state.records.get_mut(&key) {
            if record.response == Some(CreditExpiryReminderResponse::MuteVoucher)
                || record.notices >= 6
                || band_hours >= record.last_band_hours
            {
                if changed {
                    self.write(&state)?;
                }
                return Ok(None);
            }
            record.last_band_hours = band_hours;
            record.notices += 1;
            record.response = None;
            record.notices
        } else {
            if state.records.len() >= MAX_RECORDS {
                return Ok(None);
            }
            state.records.insert(
                key.clone(),
                ReminderRecord {
                    expires_at: candidate.expires_at,
                    last_band_hours: band_hours,
                    notices: 1,
                    response: None,
                },
            );
            1
        };
        self.write(&state)?;
        Ok(Some(CreditExpiryReminder {
            key,
            band_hours,
            notice_number,
            expires_at: candidate.expires_at,
        }))
    }

    /// Applies a response to the exact reservation. Empty, cancelled or timed-out
    /// questions need no response: the reservation remains counted, without spend.
    pub fn respond(
        &self,
        reminder: &CreditExpiryReminder,
        response: CreditExpiryReminderResponse,
        now: i64,
    ) -> io::Result<()> {
        let Some(_lock) = crate::account_file::try_lock(&self.home)? else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Account metadata is busy; retry the reminder response",
            ));
        };
        let mut state = self.read()?;
        let record = state
            .records
            .get_mut(&reminder.key)
            .filter(|record| {
                record.notices == reminder.notice_number
                    && record.last_band_hours == reminder.band_hours
            })
            .ok_or_else(|| io::Error::other("This expiry reminder is no longer current"))?;
        if record.response != Some(CreditExpiryReminderResponse::MuteVoucher) {
            record.response = Some(response);
        }
        if chrono::DateTime::from_timestamp(now, /*nsecs*/ 0).is_some() {
            state.last_observed_at = state.last_observed_at.max(now);
        }
        self.write(&state)
    }

    fn read(&self) -> io::Result<ReminderState> {
        let file = match fs::File::open(self.home.join(STATE_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ReminderState::default());
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::other(
                "Reset-credit reminder history exceeds its size limit",
            ));
        }
        let state: ReminderState = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("Invalid reset-credit reminder history"))?;
        if state.version != 1
            || state.last_observed_at < 0
            || chrono::DateTime::from_timestamp(state.last_observed_at, /*nsecs*/ 0).is_none()
            || state.records.len() > MAX_RECORDS
            || state.records.iter().any(|(key, record)| {
                key.len() != 64
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || !BANDS.contains(&record.last_band_hours)
                    || !(1..=6).contains(&record.notices)
                    || chrono::DateTime::from_timestamp(record.expires_at, /*nsecs*/ 0).is_none()
            })
        {
            return Err(io::Error::other("Invalid reset-credit reminder history"));
        }
        Ok(state)
    }

    fn write(&self, state: &ReminderState) -> io::Result<()> {
        let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::other(
                "Reset-credit reminder history exceeds its size limit",
            ));
        }
        let temporary = self
            .home
            .join(format!(".{STATE_FILE}-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            // rename replaces atomically on supported platforms. Never delete the
            // original on failure, which would lose the budget on a Windows crash.
            fs::rename(&temporary, self.home.join(STATE_FILE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[derive(Serialize, Deserialize)]
struct ReminderState {
    version: u8,
    last_observed_at: i64,
    records: BTreeMap<String, ReminderRecord>,
}

impl Default for ReminderState {
    fn default() -> Self {
        Self {
            version: 1,
            last_observed_at: 0,
            records: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ReminderRecord {
    expires_at: i64,
    last_band_hours: u8,
    notices: u8,
    response: Option<CreditExpiryReminderResponse>,
}

#[cfg(test)]
#[path = "credit_expiry_reminders_tests.rs"]
mod tests;
