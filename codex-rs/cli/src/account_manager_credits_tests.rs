use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

#[test]
fn reviewed_reset_releases_only_the_exact_cached_binding() {
    let mut pending = HashMap::new();
    for (profile, owner, key, credit) in [
        ("reviewed-profile", "owner", "operation", "credit"),
        ("another-owner", "other-owner", "operation", "credit"),
        ("another-key", "owner", "other-operation", "credit"),
        ("another-credit", "owner", "operation", "other-credit"),
    ] {
        pending.insert(
            profile.into(),
            PendingRedemption {
                owner_key: owner.into(),
                operation_id: key.into(),
                credit_id: credit.into(),
            },
        );
    }
    let review = PendingRedemption {
        owner_key: "owner".into(),
        operation_id: "operation".into(),
        credit_id: "credit".into(),
    };
    clear_completed(&mut pending, &review);
    let mut remaining = pending.keys().map(String::as_str).collect::<Vec<_>>();
    remaining.sort_unstable();
    assert_eq!(
        remaining,
        vec!["another-credit", "another-key", "another-owner"]
    );
}
