use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn diagnostic_uses_only_known_bounded_fields_and_records_connection_observations() {
    let extensions = ClientMcpExtensions::new([(
        MCP_APP_UI_EXTENSION_ID.into(),
        json!({
            "mimeTypes": ["text/x-dil;profile=mcp-app", "text/html;profile=mcp-app"],
            "privateValue": "do-not-print-this"
        }),
    )]);
    let mut capabilities = NativeAccountCapabilities::capture(
        "codex_chatgpt_ios_remote\n",
        "0.1",
        /*_experimental_api*/ false,
        &extensions,
    );
    capabilities.question = QuestionObservation::Responded;
    insta::assert_snapshot!(capabilities.describe(NativeAccountLanguage::English), @r"
    In-app controls
    Client: codex_chatgpt_ios_remote · 0.1
    Native questions: Answer received on this connection
    MCP Apps: Declared
    MIME types: text/x-dil;profile=mcp-app, text/html;profile=mcp-app

    Open: /account or /account manage [en|zh-CN]
    Menu descriptions show account data. Changes and credit use require explicit confirmation.
    A response confirms the protocol round trip; mobile rendering still needs a device check.
    ");
}

#[test]
fn malformed_declaration_and_timeout_do_not_claim_global_mobile_incompatibility() {
    let extensions =
        ClientMcpExtensions::new([(MCP_APP_UI_EXTENSION_ID.into(), json!({"mimeTypes": [1]}))]);
    let mut capabilities = NativeAccountCapabilities::capture(
        "remote",
        "1",
        /*_experimental_api*/ true,
        &extensions,
    );
    capabilities.question = QuestionObservation::NoAnswer;
    assert_eq!(capabilities.ui, UiDeclaration::Malformed);
    assert!(
        capabilities
            .describe(NativeAccountLanguage::English)
            .contains("No answer received")
    );
    assert!(
        !capabilities
            .describe(NativeAccountLanguage::English)
            .contains("Unsupported")
    );
}

#[test]
fn capability_input_is_bounded_and_bidi_controls_cannot_spoof_diagnostics() {
    let extensions = ClientMcpExtensions::new([(
        MCP_APP_UI_EXTENSION_ID.into(),
        json!({
            "mimeTypes": (0..20).map(|index| format!("text/{index}{}", "x".repeat(300))).collect::<Vec<_>>()
        }),
    )]);
    let capabilities = NativeAccountCapabilities::capture(
        &format!("\u{202e}{}\n", "z".repeat(300)),
        "1",
        /*_experimental_api*/ false,
        &extensions,
    );
    assert_eq!(capabilities.mime_types.len(), 8);
    assert!(
        capabilities
            .mime_types
            .iter()
            .all(|mime| mime.chars().count() <= 96)
    );
    assert!(capabilities.client_name.chars().count() <= 80);
    assert!(!capabilities.client_name.contains('\u{202e}'));
}
