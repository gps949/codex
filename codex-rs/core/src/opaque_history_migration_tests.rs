use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_login::AccountProfileId;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

use super::AccountTransitionReadiness;
use super::AccountTransitionTargetProfile;
use super::preflight_account_transition;

fn profile_id(value: &str) -> AccountProfileId {
    AccountProfileId::new(value).expect("valid test profile id")
}

fn target(profile: Option<&str>, legacy_owner: Option<&str>) -> AccountTransitionTargetProfile {
    AccountTransitionTargetProfile {
        profile_id: profile.map(profile_id),
        legacy_unattributed_profile_id: legacy_owner.map(profile_id),
        stock_execution: profile.is_none(),
    }
}

fn opaque_compaction(owner_profile_id: Option<&str>) -> ResponseItemEnvelope {
    ResponseItemEnvelope {
        item: ResponseItem::Compaction {
            id: None,
            encrypted_content: "opaque-checkpoint".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        metadata: owner_profile_id.map(|profile_id| CodexHarnessMetadata {
            execution_profile_id: Some(profile_id.to_string()),
            execution_generation: Some(4),
            ..CodexHarnessMetadata::default()
        }),
    }
}

#[test]
fn opaque_history_preflight_classifies_owner_readiness() {
    let cases = [
        (Some("a"), target(Some("b"), None), Some(Some("a"))),
        (
            None,
            target(Some("b"), Some("legacy-root")),
            Some(Some("legacy-root")),
        ),
        (None, target(Some("b"), None), Some(None)),
        (None, target(None, None), None),
    ];

    for (owner, target_profile, expected_owner) in cases {
        let actual_owner =
            match preflight_account_transition(&[opaque_compaction(owner)], &target_profile) {
                AccountTransitionReadiness::Ready => None,
                AccountTransitionReadiness::MigrationRequired { owner_profile_id } => {
                    Some(owner_profile_id.map(|profile_id| profile_id.to_string()))
                }
            };
        assert_eq!(
            actual_owner,
            expected_owner.map(|owner| owner.map(str::to_string)),
        );
    }
}
