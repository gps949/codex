use std::time::Duration;

use codex_async_utils::OrCancelExt;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use super::RecoveryWaitBudget;
use super::SamplingRecoveryBudget;

#[test]
fn sampling_recovery_budget_does_not_renew_when_execution_identity_changes() {
    let mut budget = SamplingRecoveryBudget::new(/*profile_count*/ 1);
    assert!(budget.can_retry());
    for _ in 0..3 {
        budget.record_recovery();
    }
    assert!(!budget.can_retry());
    assert!(SamplingRecoveryBudget::new(/*profile_count*/ 1).can_retry());
}

#[test]
fn oversized_pools_have_a_bounded_no_progress_recovery_allowance() {
    let mut budget = SamplingRecoveryBudget::new(/*profile_count*/ 1_000);
    for _ in 0..16 {
        assert!(budget.can_retry());
        budget.record_recovery();
    }
    assert!(!budget.can_retry());
}

#[tokio::test(start_paused = true)]
async fn repeated_waits_share_allowance_without_charging_productive_time() {
    let budget = RecoveryWaitBudget::new(Duration::from_secs(10));
    let first_wait = budget.begin_wait();
    tokio::time::sleep(Duration::from_secs(4)).await;
    drop(first_wait);
    assert_eq!(budget.remaining(), Duration::from_secs(6));

    tokio::time::advance(Duration::from_secs(120)).await;
    let second_wait = budget.begin_wait();
    tokio::time::sleep(Duration::from_secs(2)).await;
    drop(second_wait);
    assert_eq!(budget.remaining(), Duration::from_secs(4));
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_wait_future_debits_elapsed_wait() {
    let budget = RecoveryWaitBudget::new(Duration::from_secs(10));
    let cancellation = CancellationToken::new();
    let waiting = async {
        let _wait = budget.begin_wait();
        std::future::pending::<()>().await;
    }
    .or_cancel(&cancellation);
    let cancel = async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        cancellation.cancel();
    };

    let (result, ()) = tokio::join!(waiting, cancel);

    assert!(result.is_err());
    assert_eq!(budget.remaining(), Duration::from_secs(7));
}

#[tokio::test(start_paused = true)]
async fn elapsed_wait_cannot_underflow_remaining_allowance() {
    let budget = RecoveryWaitBudget::new(Duration::from_secs(10));
    let waiting = budget.begin_wait();
    tokio::time::advance(Duration::from_secs(12)).await;
    drop(waiting);

    assert_eq!(budget.remaining(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn overlapping_waits_cannot_reserve_the_same_allowance() {
    let budget = RecoveryWaitBudget::new(Duration::from_secs(10));
    let first_wait = budget.begin_wait();
    let second_wait = budget.begin_wait();
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(second_wait);
    assert_eq!(budget.remaining(), Duration::ZERO);
    drop(first_wait);

    assert_eq!(budget.remaining(), Duration::from_secs(7));
}
