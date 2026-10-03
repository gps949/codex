use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::Barrier;

const NOW: i64 = 1_800_000_000;

fn candidate(remaining_hours: i64) -> CreditExpiryCandidate<'static> {
    CreditExpiryCandidate {
        account_id: "account-fixture",
        chatgpt_user_id: "user-fixture",
        credit_id: "credit-fixture",
        reset_type: "codex_rate_limits",
        status: "available",
        expires_at: NOW + remaining_hours * 3_600,
    }
}

fn decision(reminder: Option<CreditExpiryReminder>) -> Option<(u8, u8, i64)> {
    reminder.map(|reminder| {
        (
            reminder.band_hours,
            reminder.notice_number,
            reminder.expires_at,
        )
    })
}

#[test]
fn restart_and_duplicate_profiles_share_one_claim_budget() {
    let home = tempfile::tempdir().unwrap();
    let first = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let duplicate = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(23);
    assert_eq!(
        decision(first.try_claim(&credit, NOW).unwrap()),
        Some((24, 1, credit.expires_at))
    );
    assert_eq!(duplicate.try_claim(&credit, NOW).unwrap(), None);
    drop(first);
    let restarted = CreditExpiryReminderStore::new(home.path().to_path_buf());
    assert_eq!(restarted.try_claim(&credit, NOW + 60).unwrap(), None);
}

#[test]
fn concurrent_store_claims_present_at_most_once() {
    let home = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let home = home.path().to_path_buf();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let store = CreditExpiryReminderStore::new(home);
            barrier.wait();
            store.try_claim(&candidate(23), NOW).unwrap()
        }));
    }
    barrier.wait();
    let claims: Vec<_> = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(claims.len(), 1);
}

#[test]
fn account_user_and_voucher_isolate_notice_budgets() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let original = candidate(23);
    let other_account = CreditExpiryCandidate {
        account_id: "second-account",
        ..original
    };
    let other_user = CreditExpiryCandidate {
        chatgpt_user_id: "second-user",
        ..original
    };
    let other_voucher = CreditExpiryCandidate {
        credit_id: "second-credit",
        ..original
    };
    assert_eq!(
        [original, other_account, other_user, other_voucher]
            .map(|credit| decision(store.try_claim(&credit, NOW).unwrap())),
        [Some((24, 1, original.expires_at)); 4]
    );
}

#[test]
fn rejects_unavailable_unknown_scope_expired_or_invalid_identifiers() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let valid = candidate(23);
    let too_long = "v".repeat(257);
    let rejected = [
        CreditExpiryCandidate {
            reset_type: "",
            ..valid
        },
        CreditExpiryCandidate {
            reset_type: "chatgpt_rate_limits",
            ..valid
        },
        CreditExpiryCandidate {
            status: "redeemed",
            ..valid
        },
        CreditExpiryCandidate {
            credit_id: " ",
            ..valid
        },
        CreditExpiryCandidate {
            credit_id: &too_long,
            ..valid
        },
        CreditExpiryCandidate {
            account_id: "",
            ..valid
        },
        CreditExpiryCandidate {
            chatgpt_user_id: "",
            ..valid
        },
        CreditExpiryCandidate {
            expires_at: NOW,
            ..valid
        },
        CreditExpiryCandidate {
            expires_at: i64::MAX,
            ..valid
        },
        candidate(25),
    ];
    for credit in rejected {
        assert_eq!(store.try_claim(&credit, NOW).unwrap(), None);
    }
    assert_eq!(store.try_claim(&valid, i64::MIN).unwrap(), None);
    assert!(!home.path().join(STATE_FILE).exists());
}

#[test]
fn first_detection_uses_current_band_without_replaying_missed_bands() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(4);
    assert_eq!(
        decision(store.try_claim(&credit, NOW).unwrap()),
        Some((6, 1, credit.expires_at))
    );
    assert_eq!(store.try_claim(&credit, NOW + 60).unwrap(), None);
    assert_eq!(
        decision(store.try_claim(&credit, NOW + 3_600).unwrap()),
        Some((3, 2, credit.expires_at))
    );
}

#[test]
fn abandoned_claim_is_not_released_and_backwards_clock_cannot_repeat_it() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(20);
    drop(store.try_claim(&credit, NOW).unwrap());
    assert_eq!(
        decision(store.try_claim(&credit, NOW + 3 * 3_600).unwrap()),
        Some((18, 2, credit.expires_at))
    );
    assert_eq!(store.try_claim(&credit, NOW).unwrap(), None);
    assert_eq!(store.try_claim(&credit, NOW + 2 * 3_600).unwrap(), None);
}

#[test]
fn a_clock_rollback_cannot_reopen_history_pruned_after_the_grace_period() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let original = candidate(23);
    store.try_claim(&original, NOW).unwrap().unwrap();
    let later = original.expires_at + RETENTION_GRACE_SECONDS + 60;
    let other = CreditExpiryCandidate {
        credit_id: "later-credit",
        expires_at: later + 3_600,
        ..original
    };
    store.try_claim(&other, later).unwrap().unwrap();
    assert_eq!(store.read().unwrap().records.len(), 1);
    assert_eq!(store.try_claim(&original, NOW).unwrap(), None);
}

#[test]
fn expiry_extension_keeps_claim_count_and_skipped_bands() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let original = candidate(4);
    assert_eq!(
        decision(store.try_claim(&original, NOW).unwrap()),
        Some((6, 1, original.expires_at))
    );
    let extended = candidate(23);
    assert_eq!(store.try_claim(&extended, NOW).unwrap(), None);
    assert_eq!(store.try_claim(&extended, NOW + 18 * 3_600).unwrap(), None);
    assert_eq!(
        decision(store.try_claim(&extended, NOW + 20 * 3_600).unwrap()),
        Some((3, 2, extended.expires_at))
    );
}

#[test]
fn observing_a_long_extension_retains_the_budget_before_history_cleanup() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let original = candidate(23);
    store.try_claim(&original, NOW).unwrap().unwrap();
    let extended = CreditExpiryCandidate {
        expires_at: NOW + 40 * 24 * 3_600,
        ..original
    };
    assert_eq!(store.try_claim(&extended, NOW + 60).unwrap(), None);
    let later = extended.expires_at - 3_600;
    assert_eq!(
        decision(store.try_claim(&extended, later).unwrap()),
        Some((1, 2, extended.expires_at))
    );
}

#[test]
fn repeated_expiry_extensions_never_reopen_the_six_notice_budget() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let mut decisions = Vec::new();
    for remaining in [23, 17, 11, 5, 2, 0] {
        let credit = CreditExpiryCandidate {
            expires_at: NOW + remaining * 3_600 + 60,
            ..candidate(23)
        };
        decisions.push(decision(store.try_claim(&credit, NOW).unwrap()));
    }
    assert_eq!(
        decisions,
        vec![
            Some((24, 1, NOW + 23 * 3_600 + 60)),
            Some((18, 2, NOW + 17 * 3_600 + 60)),
            Some((12, 3, NOW + 11 * 3_600 + 60)),
            Some((6, 4, NOW + 5 * 3_600 + 60)),
            Some((3, 5, NOW + 2 * 3_600 + 60)),
            Some((1, 6, NOW + 60)),
        ]
    );
    for remaining in [24, 18, 12, 6, 3, 1] {
        assert_eq!(store.try_claim(&candidate(remaining), NOW).unwrap(), None);
    }
}

#[test]
fn snooze_waits_for_next_stage_and_mute_survives_restart_and_extension() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(23);
    let first = store.try_claim(&credit, NOW).unwrap().unwrap();
    store
        .respond(
            &first,
            CreditExpiryReminderResponse::SnoozeUntilNextStage,
            NOW,
        )
        .unwrap();
    assert_eq!(store.try_claim(&credit, NOW + 60).unwrap(), None);
    let next = store.try_claim(&credit, NOW + 6 * 3_600).unwrap().unwrap();
    store
        .respond(
            &next,
            CreditExpiryReminderResponse::MuteVoucher,
            NOW + 6 * 3_600,
        )
        .unwrap();
    let restarted = CreditExpiryReminderStore::new(home.path().to_path_buf());
    assert_eq!(
        restarted.try_claim(&credit, NOW + 12 * 3_600).unwrap(),
        None
    );
    let extended = CreditExpiryCandidate {
        expires_at: credit.expires_at + 3_600,
        ..credit
    };
    assert_eq!(
        restarted.try_claim(&extended, NOW + 23 * 3_600).unwrap(),
        None
    );
}

#[test]
fn stale_answer_does_not_replace_a_newer_notice_response() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(23);
    let first = store.try_claim(&credit, NOW).unwrap().unwrap();
    let next = store.try_claim(&credit, NOW + 6 * 3_600).unwrap().unwrap();
    store
        .respond(
            &next,
            CreditExpiryReminderResponse::MuteVoucher,
            NOW + 6 * 3_600,
        )
        .unwrap();
    assert!(
        store
            .respond(
                &first,
                CreditExpiryReminderResponse::SnoozeUntilNextStage,
                NOW + 6 * 3_600
            )
            .is_err()
    );
    assert_eq!(store.try_claim(&credit, NOW + 18 * 3_600).unwrap(), None);
}

#[test]
fn repeated_response_cannot_unmute_the_same_voucher() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(23);
    let reminder = store.try_claim(&credit, NOW).unwrap().unwrap();
    store
        .respond(&reminder, CreditExpiryReminderResponse::MuteVoucher, NOW)
        .unwrap();
    store
        .respond(
            &reminder,
            CreditExpiryReminderResponse::SnoozeUntilNextStage,
            NOW,
        )
        .unwrap();
    assert_eq!(store.try_claim(&credit, NOW + 6 * 3_600).unwrap(), None);
}

#[test]
fn persisted_metadata_has_no_raw_owner_or_voucher_identifiers() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let credit = candidate(23);
    store.try_claim(&credit, NOW).unwrap().unwrap();
    let contents = std::fs::read_to_string(home.path().join(STATE_FILE)).unwrap();
    for identifier in [credit.account_id, credit.chatgpt_user_id, credit.credit_id] {
        assert!(!contents.contains(identifier));
    }
    let state = store.read().unwrap();
    assert_eq!(state.records.len(), 1);
    assert_eq!(state.records.keys().next().unwrap().len(), 64);
}

#[test]
fn live_backlog_is_bounded_and_expired_records_are_reclaimed_after_grace() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let mut state = ReminderState::default();
    for index in 0..MAX_RECORDS {
        state.records.insert(
            format!("{index:064x}"),
            ReminderRecord {
                expires_at: NOW + 60,
                last_band_hours: 1,
                notices: 1,
                response: None,
            },
        );
    }
    store.write(&state).unwrap();
    assert_eq!(store.try_claim(&candidate(23), NOW).unwrap(), None);
    assert_eq!(store.read().unwrap().records.len(), MAX_RECORDS);
    let later = NOW + RETENTION_GRACE_SECONDS + 61;
    let credit = CreditExpiryCandidate {
        expires_at: later + 3_600,
        ..candidate(23)
    };
    assert_eq!(
        decision(store.try_claim(&credit, later).unwrap()),
        Some((1, 1, credit.expires_at))
    );
    assert_eq!(store.read().unwrap().records.len(), 1);
    assert!(
        std::fs::metadata(home.path().join(STATE_FILE))
            .unwrap()
            .len()
            <= MAX_STATE_BYTES
    );
}

#[test]
fn corrupt_or_oversized_history_never_silently_resets_budget() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let path = home.path().join(STATE_FILE);
    std::fs::write(&path, b"broken state").unwrap();
    assert!(store.try_claim(&candidate(23), NOW).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"broken state");
    std::fs::write(&path, vec![b' '; (MAX_STATE_BYTES + 1) as usize]).unwrap();
    assert!(store.try_claim(&candidate(23), NOW).is_err());
}

#[test]
fn busy_account_transaction_never_blocks_or_claims_a_reminder() {
    let home = tempfile::tempdir().unwrap();
    let store = CreditExpiryReminderStore::new(home.path().to_path_buf());
    let lock = crate::account_file::lock(home.path()).unwrap();
    assert_eq!(store.try_claim(&candidate(23), NOW).unwrap(), None);
    drop(lock);
    assert_eq!(
        decision(store.try_claim(&candidate(23), NOW).unwrap()),
        Some((24, 1, candidate(23).expires_at))
    );
}
