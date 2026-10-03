use super::Locale;
use pretty_assertions::assert_eq;

#[test]
fn interpolation_preserves_account_names_and_operation_tokens() {
    let account = "Needs login {} REVIEWED 中文";
    assert_eq!(
        Locale::SimplifiedChinese.format("This consumes one reset credit for {}.", &[account]),
        format!("此操作会为 {account} 使用一张重置券。")
    );
    assert_eq!(
        Locale::English.format("This consumes one reset credit for {}.", &[account]),
        format!("This consumes one reset credit for {account}.")
    );
}

#[test]
fn unknown_backend_diagnostics_stay_intact() {
    let diagnostic = "Provider said: account=Needs login model=available op={RETRY} code=502";
    assert_eq!(Locale::SimplifiedChinese.message(diagnostic), diagnostic);
    assert_eq!(Locale::SimplifiedChinese.notice(diagnostic), diagnostic);
}

#[test]
fn quota_summary_localization_requires_the_complete_numeric_backend_shape() {
    let summary =
        "Quota check: 12 updated, 3 failed, 4 already checking. Each account shows its own result.";
    assert_eq!(
        Locale::SimplifiedChinese.notice(summary),
        "额度检查：12 个已更新，3 个失败，4 个正在检查。每个账号显示各自的结果。"
    );
    assert_eq!(Locale::English.notice(summary), summary);
    let diagnostic = "Quota check: Needs login updated, 3 failed, 4 already checking. Each account shows its own result.";
    assert_eq!(Locale::SimplifiedChinese.notice(diagnostic), diagnostic);
}
