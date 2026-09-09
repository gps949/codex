use super::warmup_model_info;
use pretty_assertions::assert_eq;

#[test]
fn warmup_model_info_uses_requested_slug() {
    let info = warmup_model_info("gpt-5.2-codex");
    assert_eq!(info.slug, "gpt-5.2-codex");
    assert_eq!(info.display_name, "gpt-5.2-codex");
}
