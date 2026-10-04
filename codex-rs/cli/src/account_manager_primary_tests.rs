use super::*;
use codex_app_server::account_management::PrimaryLoginView;

#[test]
fn manager_host_page_distinguishes_saved_credentials_from_remote_connection() {
    let inventory = AccountManagerInventory {
        host_now: 1800000000,
        primary_login: Some(PrimaryLoginView {
            source: "profile".into(),
            profile_id: Some("fixture-host".into()),
            label: "Personal".into(),
            email: Some("alice@example.com".into()),
            ready: true,
            status: "storedReady".into(),
            revision: 4,
            runtime: None,
            message: Some("Host sign-in and inference selection are independent.".into()),
        }),
        paused: false,
        active_profile_id: None,
        accounts: Vec::new(),
        settings: serde_json::Value::Null,
        login_jobs: Vec::new(),
        api_accounts: Vec::new(),
        api_selection: codex_login::ApiAccountSelection::Subscription,
        api_fallback: codex_login::ApiAccountFallback::default(),
    };
    insta::assert_snapshot!(format!(
        "English:\n{}\nChinese:\n{}",
        render(&inventory, Locale::English),
        render(&inventory, Locale::SimplifiedChinese),
    ));
}
