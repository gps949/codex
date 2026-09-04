//! Process-local single-flight coordination for automatic reset-credit redemption.
//!
//! Account-pool generations are globally monotonic. Keeping the latest finished generation closes
//! ambiguous retries for the same failure, while a later generation can start one new attempt.
//! The leader owns completion notification so cancellation wakes every concurrent follower.

use std::sync::Mutex;

use codex_login::AccountProfileId;
use tokio::sync::watch;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResetCreditRescueAttemptStatus {
    Pending,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResetCreditRescueAttemptKey {
    pub(crate) profile_id: AccountProfileId,
    pub(crate) generation: u64,
}

struct ResetCreditRescueAttemptState {
    key: ResetCreditRescueAttemptKey,
    completion: watch::Sender<ResetCreditRescueAttemptStatus>,
}

pub(crate) enum ResetCreditRescueAttempt {
    Leader(ResetCreditRescueLeader),
    Follower(ResetCreditRescueFollower),
    AlreadyFinished,
}

pub(crate) struct ResetCreditRescueLeader {
    redeem_request_id: String,
    completion: watch::Sender<ResetCreditRescueAttemptStatus>,
}

impl ResetCreditRescueLeader {
    pub(crate) fn redeem_request_id(&self) -> &str {
        &self.redeem_request_id
    }
}

impl Drop for ResetCreditRescueLeader {
    fn drop(&mut self) {
        self.completion
            .send_replace(ResetCreditRescueAttemptStatus::Finished);
    }
}

pub(crate) struct ResetCreditRescueFollower {
    completion: watch::Receiver<ResetCreditRescueAttemptStatus>,
}

impl ResetCreditRescueFollower {
    pub(crate) async fn wait(mut self) {
        while *self.completion.borrow_and_update() != ResetCreditRescueAttemptStatus::Finished {
            if self.completion.changed().await.is_err() {
                break;
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct ResetCreditRescueSingleflight {
    current: Mutex<Option<ResetCreditRescueAttemptState>>,
}

impl ResetCreditRescueSingleflight {
    pub(crate) fn begin(&self, key: ResetCreditRescueAttemptKey) -> ResetCreditRescueAttempt {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(attempt) = current.as_ref() {
            if *attempt.completion.borrow() == ResetCreditRescueAttemptStatus::Pending {
                return ResetCreditRescueAttempt::Follower(ResetCreditRescueFollower {
                    completion: attempt.completion.subscribe(),
                });
            }
            debug_assert!(
                key.generation != attempt.key.generation
                    || key.profile_id == attempt.key.profile_id,
                "one account-pool generation must identify one execution profile"
            );
            if key.generation <= attempt.key.generation {
                return ResetCreditRescueAttempt::AlreadyFinished;
            }
        }

        let (completion, _completion_rx) = watch::channel(ResetCreditRescueAttemptStatus::Pending);
        *current = Some(ResetCreditRescueAttemptState {
            key,
            completion: completion.clone(),
        });
        ResetCreditRescueAttempt::Leader(ResetCreditRescueLeader {
            redeem_request_id: uuid::Uuid::new_v4().to_string(),
            completion,
        })
    }
}

#[cfg(test)]
#[path = "reset_credit_singleflight_tests.rs"]
mod tests;
