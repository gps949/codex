use codex_login::AccountProfileId;

use super::*;

fn attempt_key(generation: u64) -> ResetCreditRescueAttemptKey {
    ResetCreditRescueAttemptKey {
        profile_id: AccountProfileId::new("failed-profile").expect("valid profile id"),
        generation,
    }
}

#[tokio::test]
async fn dropping_leader_wakes_followers_and_closes_the_generation() {
    let singleflight = ResetCreditRescueSingleflight::default();
    let key = attempt_key(/*generation*/ 7);
    let ResetCreditRescueAttempt::Leader(leader) = singleflight.begin(key.clone()) else {
        panic!("first attempt must lead");
    };
    let ResetCreditRescueAttempt::Follower(follower) = singleflight.begin(key.clone()) else {
        panic!("concurrent attempt must follow");
    };

    drop(leader);
    tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 1), follower.wait())
        .await
        .expect("leader cancellation must wake followers");

    assert!(matches!(
        singleflight.begin(key),
        ResetCreditRescueAttempt::AlreadyFinished
    ));
    assert!(matches!(
        singleflight.begin(attempt_key(/*generation*/ 8)),
        ResetCreditRescueAttempt::Leader(_)
    ));
}
