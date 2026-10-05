use super::*;
use codex_app_server::account_management::ApiAccountView;
use pretty_assertions::assert_eq;

fn api_account() -> ApiAccountView {
    ApiAccountView {
        account: codex_login::ApiAccount {
            id: "api-fixture".into(),
            label: "API fixture".into(),
            base_url: "https://provider.example/v1".into(),
            model: "fixture-model".into(),
            disabled: false,
            context_window: 32_768,
            images: false,
        },
        has_key: true,
        credential_revision: Some("confirmed-fixture-revision".into()),
    }
}

#[test]
fn api_confirmation_captures_the_revision_before_the_account_view_changes() -> anyhow::Result<()> {
    let mut account = api_account();
    let revision = api_confirmation_revision(&account, Locale::English)?;
    account.credential_revision = Some("replacement-fixture-revision".into());
    assert_eq!(revision, "confirmed-fixture-revision");
    Ok(())
}

#[test]
fn api_confirmation_rejects_missing_revision_or_unavailable_credentials() {
    for revision in [None, Some("")] {
        let mut account = api_account();
        account.credential_revision = revision.map(str::to_owned);
        assert!(api_confirmation_revision(&account, Locale::English).is_err());
    }
    for (has_key, disabled) in [(false, false), (true, true)] {
        let mut account = api_account();
        account.has_key = has_key;
        account.account.disabled = disabled;
        assert!(api_confirmation_revision(&account, Locale::English).is_err());
    }
}

#[test]
fn api_confirmation_shows_the_exact_provider_target() {
    insta::assert_snapshot!(format!("{}\n---\n{}", api_confirmation_details(&api_account(), Locale::English), api_confirmation_details(&api_account(), Locale::SimplifiedChinese)), @r###"
    API account: API fixture
    Profile: api-fixture
    HTTPS endpoint: https://provider.example/v1
    Model: fixture-model
    ---
    API 账号：API fixture
    档案：api-fixture
    HTTPS 接口：https://provider.example/v1
    模型：fixture-model
    "###);
}
