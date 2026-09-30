use std::sync::Mutex;
use std::sync::mpsc;

use codex_core::ExecutionAccountPoolHandle;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_login::AccountProfileId;
use codex_login::CodexAuth;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::latest_compaction_summary_execution_provenance;
use super::account_failover::write_account_pool_fixture;

struct GatedCompaction {
    started: mpsc::Sender<()>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Respond for GatedCompaction {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        self.started.send(()).expect("signal compaction request");
        self.release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .expect("compaction release receiver")
            .recv()
            .expect("release compaction response");
        responses::sse_response(sse(vec![
            ev_response_created("bound-compaction"),
            ev_assistant_message("bound-summary", "Captured primary account summary"),
            ev_completed("bound-compaction"),
        ]))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn portable_manual_compaction_keeps_captured_auth_when_the_pool_switches()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let seed = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("seed-message", "Primary history"),
            ev_completed("seed-response"),
        ]),
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_pre_build_hook(write_account_pool_fixture)
        .with_config(|config| {
            config.model_provider.supports_websockets = false;
            config.features.disable(Feature::TokenBudget).unwrap();
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture.submit_turn("Capture primary history").await?;
    assert_eq!(
        seed.single_request().header("authorization"),
        Some("Bearer access-primary".to_owned()),
    );
    let pool = ExecutionAccountPoolHandle::shared(fixture.thread_manager.auth_manager());
    let primary_identity = pool.active_identity().expect("primary execution identity");
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(GatedCompaction {
            started: started_tx,
            release: Mutex::new(Some(release_rx)),
        })
        .expect(/*requests*/ 1)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let compact_turn_id = fixture.codex.submit(Op::Compact).await?;
    tokio::task::spawn_blocking(move || started_rx.recv()).await??;
    let backup_identity = pool
        .activate(&AccountProfileId::new("backup-acct")?, /*force*/ false)
        .await?;
    release_tx.send(())?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    fixture.codex.flush_rollout().await?;

    let requests = responses::received_responses_requests(&server).await;
    let compact_request = requests.last().expect("captured compaction request");
    assert_eq!(
        (
            compact_request.header("authorization"),
            compact_request.header("chatgpt-account-id"),
        ),
        (
            Some("Bearer access-primary".to_owned()),
            Some("account-primary-acct".to_owned()),
        ),
    );
    let body = compact_request.body_json();
    let compact_metadata: serde_json::Value = serde_json::from_str(
        &compact_request
            .header("x-codex-turn-metadata")
            .expect("turn metadata header"),
    )?;
    assert_eq!(
        (
            compact_metadata["turn_id"].clone(),
            compact_metadata["compaction"]["implementation"].clone(),
            compact_metadata["compaction"]["phase"].clone(),
        ),
        (
            json!(compact_turn_id),
            json!("responses"),
            json!("standalone_turn")
        ),
    );
    assert!(body["input"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|part| part["text"] == codex_core::compact::SUMMARIZATION_PROMPT)
            })
        })
    }));
    let rollout = fixture.session_configured.rollout_path.as_ref().unwrap();
    assert_eq!(
        latest_compaction_summary_execution_provenance(rollout)?,
        (
            Some(primary_identity.profile_id.to_string()),
            Some(primary_identity.generation)
        ),
    );
    let summary_metadata = std::fs::read_to_string(rollout)?
        .lines()
        .filter_map(|line| codex_rollout::parse_rollout_line(line).ok())
        .find_map(|line| match line.item {
            RolloutItem::ResponseItem(envelope)
                if matches!(&envelope.item, ResponseItem::Message { role, content, .. }
                    if role == "assistant" && content.iter().any(|part|
                        matches!(part, ContentItem::OutputText { text }
                            if text == "Captured primary account summary"))) =>
            {
                envelope.metadata
            }
            _ => None,
        })
        .expect("committed compaction output metadata");
    assert!(summary_metadata.compaction_output);
    assert_eq!(
        (
            summary_metadata.execution_profile_id,
            summary_metadata.execution_generation
        ),
        (
            Some(primary_identity.profile_id.to_string()),
            Some(primary_identity.generation)
        ),
    );
    assert_eq!(pool.active_identity().unwrap(), backup_identity);
    Ok(())
}

#[tokio::test]
async fn stock_manual_compaction_streams_v2_and_installs_the_checkpoint() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.model_provider.supports_websockets = false;
            config.features.disable(Feature::TokenBudget).unwrap();
        });
    let fixture = builder.build_with_auto_env(&server).await?;
    let compact = mount_sse_once(
        &server,
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": {"type": "compaction", "encrypted_content": "v2-summary"},
            }),
            ev_completed("v2-compact-response"),
        ]),
    )
    .await;
    fixture.codex.submit(Op::Compact).await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = compact.single_request();
    assert_eq!(
        (request.path(), request.input().last().cloned()),
        (
            "/v1/responses".to_owned(),
            Some(json!({"type": "compaction_trigger"}))
        ),
    );
    let metadata: serde_json::Value =
        serde_json::from_str(&request.header("x-codex-turn-metadata").unwrap())?;
    assert_eq!(
        metadata["compaction"],
        json!({
            "trigger": "manual",
            "reason": "user_requested",
            "implementation": "responses_compaction_v2",
            "phase": "standalone_turn",
            "strategy": "memento",
        }),
    );

    let follow_up = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("follow-up", "History retained"),
            ev_completed("follow-up-response"),
        ]),
    )
    .await;
    fixture.submit_turn("Continue after compaction").await?;
    assert_eq!(
        responses::strip_metadata_from_json(responses::strip_response_item_ids_from_json(
            serde_json::Value::Array(follow_up.single_request().inputs_of_type("compaction")),
        )),
        json!([{"type": "compaction", "encrypted_content": "v2-summary"}]),
    );
    Ok(())
}
