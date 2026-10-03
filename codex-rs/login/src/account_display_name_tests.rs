use super::account_display_name;
use pretty_assertions::assert_eq;

#[test]
fn names_follow_custom_label_then_email_then_profile_id() {
    assert_eq!(
        [
            account_display_name(Some(" Work "), Some("alice@example.com"), "profile-1"),
            account_display_name(
                /*label*/ None,
                Some(" alice@example.com "),
                "profile-1"
            ),
            account_display_name(Some(" \t "), Some("alice@example.com"), "profile-1"),
            account_display_name(Some(" "), Some("\t"), "profile-1"),
        ],
        [
            "Work",
            "alice@example.com",
            "alice@example.com",
            "profile-1"
        ]
    );
}

#[test]
fn a_derived_name_tracks_changed_email_without_becoming_a_custom_label() {
    let label = None;
    let first = account_display_name(label, Some("old@example.com"), "profile-1");
    let next = account_display_name(label, Some("new@example.com"), "profile-1");
    assert_eq!(
        (label, first, next),
        (None, "old@example.com", "new@example.com")
    );
    assert_eq!(
        account_display_name(Some("Work"), Some("new@example.com"), "profile-1"),
        "Work"
    );
}
