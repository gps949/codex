use super::*;

#[test]
fn decision_page_distinguishes_saved_settings_from_runtime_observation() {
    let view = json!({"config":{"provider":"cloudflare","mode":"shadow"},"credentialPresent":true});
    insta::assert_snapshot!(format!(
        "{}\n---\n{}",
        render(&view, Locale::English),
        render(&view, Locale::SimplifiedChinese)
    ));
}
