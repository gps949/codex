use std::path::Path;

use codex_core::TurnInputRequest;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

/// Writes one ready managed account profile: fake ChatGPT credentials in the profile's
/// credential home plus its manifest entry. Mirrors the on-disk layout produced by
/// `codex account add`.
fn profile_manifest_entry(id: &str, priority: u32) -> serde_json::Value {
    json!({
        "id": id,
        "label": null,
        "priority": priority,
        "credential_location": "managed_profile",
        "state": "ready",
        "disabled": false,
    })
}

#[expect(clippy::unwrap_used)]
fn write_profile_credentials(codex_home: &Path, id: &str, access_token: &str) {
    use base64::Engine as _;

    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header = b64(&serde_json::to_vec(&json!({ "alg": "none", "typ": "JWT" })).unwrap());
    let payload = b64(&serde_json::to_vec(&json!({
        "email": format!("{id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": format!("account-{id}"),
        }
    }))
    .unwrap());
    let fake_jwt = format!("{header}.{payload}.{}", b64(b"sig"));

    let credential_home = codex_home.join("auth-profiles").join(id);
    std::fs::create_dir_all(&credential_home).unwrap();
    std::fs::write(
        credential_home.join("auth.json"),
        serde_json::to_string_pretty(&json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": fake_jwt,
                "access_token": access_token,
                "refresh_token": format!("refresh-{id}"),
                "account_id": format!("account-{id}"),
            },
            "last_refresh": chrono::Utc::now(),
        }))
        .unwrap(),
    )
    .unwrap();
}

#[expect(clippy::unwrap_used)]
fn write_account_pool_fixture(codex_home: &Path) {
    write_profile_credentials(codex_home, "primary-acct", "access-primary");
    write_profile_credentials(codex_home, "backup-acct", "access-backup");
    std::fs::write(
        codex_home.join("account-profiles.json"),
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "profiles": [
                profile_manifest_entry("primary-acct", 0),
                profile_manifest_entry("backup-acct", 10),
            ],
        }))
        .unwrap(),
    )
    .unwrap();
}

#[expect(clippy::unwrap_used)]
fn write_malformed_account_pool_fixture(codex_home: &Path) {
    std::fs::write(
        codex_home.join("account-profiles.json"),
        "not valid account profile json",
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_account_pool_fails_closed_before_sampling() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let unexpected_sampling = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("unexpected-response"),
            ev_assistant_message("unexpected-message", "request should not be sent"),
            ev_completed("unexpected-response"),
        ]),
    )
    .await;

    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_malformed_account_pool_fixture);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let terminal = wait_for_event(&fixture.codex, |msg| {
        matches!(msg, EventMsg::Error(_) | EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::Error(error) = terminal else {
        panic!("malformed configured account pool must fail the turn");
    };
    assert!(
        error
            .message
            .contains("failed to initialize native multi-account execution"),
        "unexpected pool initialization error: {}",
        error.message,
    );
    assert!(
        unexpected_sampling.requests().is_empty(),
        "malformed configured account pool must not send a /responses request",
    );

    Ok(())
}

/// End-to-end: a usage-limit rejection on the preferred account rotates the pool to the backup
/// account, warns the user, and completes the same turn on the backup account's credentials.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_rotates_to_backup_account_and_completes_turn() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    // First sampling request: authoritative plan usage-limit rejection.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "limit reached",
                "resets_at": chrono::Utc::now().timestamp() + 3600,
                "plan_type": "pro"
            }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Replayed request after the account switch: normal completion.
    let success = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "hello from the backup account"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_pre_build_hook(write_account_pool_fixture);
    let fixture = builder.build(&server).await?;
    let codex = fixture.codex.clone();

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await?;

    let warning = wait_for_event(&codex, |msg| matches!(msg, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning) = warning else {
        unreachable!();
    };
    assert!(
        warning.message.contains("switched to `backup-acct`"),
        "unexpected switch warning: {}",
        warning.message
    );

    wait_for_event(&codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;

    // The replayed request must run on the backup account's credentials.
    let request = success.single_request();
    assert_eq!(
        request.header("authorization"),
        Some("Bearer access-backup".to_string())
    );

    // The scheduler must persist the rotation for future processes.
    let runtime_state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        fixture.home.path().join("account-runtime-state.json"),
    )?)?;
    assert_eq!(
        runtime_state["active_profile_id"],
        json!("backup-acct"),
        "unexpected runtime state: {runtime_state:#}"
    );

    Ok(())
}
