use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_core::CodexThread;
use codex_core::ExecutionAccountPoolHandle;
use codex_core::TurnInputRequest;
use codex_history::CodexHarnessMetadata;
use codex_history::CompactedItem;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_login::CodexAuth;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_failover::latest_compaction_summary_execution_provenance;
use super::account_failover::write_account_pool_fixture;
use super::account_failover::write_backup_only_account_pool_fixture;
use super::account_failover::write_profile_credentials;

#[expect(clippy::unwrap_used)]
fn write_root_credentials(codex_home: &Path) {
    write_profile_credentials(codex_home, "root-fixture", "access-root");
    std::fs::copy(
        codex_home.join("auth-profiles/root-fixture/auth.json"),
        codex_home.join("auth.json"),
    )
    .unwrap();
}

fn persisted_compactions(rollout_path: &Path) -> anyhow::Result<Vec<CompactedItem>> {
    Ok(std::fs::read_to_string(rollout_path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|line| match line.item {
            RolloutItem::Compacted(compacted) => Some(compacted),
            _ => None,
        })
        .collect())
}

fn decoded_request_body(request: &wiremock::Request) -> anyhow::Result<serde_json::Value> {
    let body = if request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("zstd"))
    {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body))?
    } else {
        request.body.clone()
    };
    Ok(serde_json::from_slice(&body)?)
}

fn append_opaque_compaction(
    rollout_path: &Path,
    owner_profile_id: Option<&str>,
    encrypted_content: &str,
) -> anyhow::Result<()> {
    let metadata = owner_profile_id.map(|profile_id| CodexHarnessMetadata {
        execution_profile_id: Some(profile_id.to_string()),
        execution_generation: Some(7),
        ..CodexHarnessMetadata::default()
    });
    let line = RolloutLine {
        timestamp: chrono::Utc::now().to_rfc3339(),
        ordinal: None,
        item: RolloutItem::Compacted(CompactedItem {
            message: String::new(),
            replacement_history: Some(vec![ResponseItemEnvelope {
                item: ResponseItem::Compaction {
                    id: None,
                    encrypted_content: encrypted_content.to_string(),
                    internal_chat_message_metadata_passthrough: None,
                },
                metadata,
            }]),
            mcp_resource_origins: None,
            window_number: None,
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
    };
    let mut rollout = std::fs::read_to_string(rollout_path)?;
    rollout.push_str(&serde_json::to_string(&line)?);
    rollout.push('\n');
    std::fs::write(rollout_path, rollout)?;
    Ok(())
}

async fn create_opaque_rollout(
    server: &MockServer,
    owner_profile_id: Option<&str>,
    encrypted_content: &str,
) -> anyhow::Result<(Arc<tempfile::TempDir>, std::path::PathBuf)> {
    mount_sse_once(
        server,
        sse(vec![
            ev_response_created("opaque-history-seed-response"),
            ev_assistant_message(
                "opaque-history-seed-message",
                "seed before opaque checkpoint",
            ),
            ev_completed("opaque-history-seed-response"),
        ]),
    )
    .await;
    let mut builder = test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let initial = builder.build_with_auto_env(server).await?;
    initial
        .submit_turn("seed history before opaque compaction")
        .await?;
    let home = Arc::clone(&initial.home);
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("opaque history rollout path");
    initial.codex.shutdown_and_wait().await?;
    if owner_profile_id.is_none() {
        write_root_credentials(home.path());
    }
    append_opaque_compaction(&rollout_path, owner_profile_id, encrypted_content)?;
    Ok((home, rollout_path))
}

async fn resume_with_profile(
    server: &MockServer,
    mut builder: TestCodexBuilder,
    home: Arc<tempfile::TempDir>,
    rollout_path: PathBuf,
    profile_id: &str,
) -> anyhow::Result<(TestCodex, ExecutionAccountPoolHandle)> {
    let test = builder
        .resume_with_auto_env(server, home, rollout_path)
        .await?;
    let pool = ExecutionAccountPoolHandle::shared(test.thread_manager.auth_manager());
    assert!(pool.ensure_from_config(&test.config).await?);
    pool.activate(
        &codex_login::AccountProfileId::new(profile_id)?,
        /*force*/ false,
    )
    .await?;
    Ok((test, pool))
}

async fn wait_for_turn_error(codex: &CodexThread) -> anyhow::Result<String> {
    let mut error_message = None;
    loop {
        match codex.next_event().await?.msg {
            EventMsg::Error(error) => error_message = Some(error.message),
            EventMsg::TurnComplete(_) => {
                return error_message
                    .ok_or_else(|| anyhow::anyhow!("turn completed without an error"));
            }
            _ => {}
        }
    }
}

async fn start_text_turn(codex: &CodexThread, text: &str) -> anyhow::Result<()> {
    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    Ok(())
}

async fn response_request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("captured response requests")
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .count()
}

fn assert_migration_error(message: &str, owner_profile_id: &str) {
    assert!(
        message.contains(owner_profile_id) && message.contains("/compact"),
        "unexpected migration error: {message}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_history_transition_user_switch_blocks_target_and_keeps_preference()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let (home, rollout_path) = create_opaque_rollout(
        &server,
        /*owner_profile_id*/ None,
        "LEGACY_ROOT_OPAQUE",
    )
    .await?;
    write_backup_only_account_pool_fixture(home.path());

    let (resumed, pool) = resume_with_profile(
        &server,
        test_codex().with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing()),
        home,
        rollout_path,
        "backup-acct",
    )
    .await?;

    let requests_before = response_request_count(&server).await;
    start_text_turn(&resumed.codex, "continue on the selected account").await?;
    let error_message = wait_for_turn_error(&resumed.codex).await?;

    assert_eq!(response_request_count(&server).await, requests_before);
    assert_eq!(
        pool.active_identity()
            .expect("selected preference remains active")
            .profile_id
            .as_str(),
        "backup-acct",
    );
    assert_migration_error(&error_message, "legacy-root");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_history_failover_records_cooldown_without_sending_target_request()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let (home, rollout_path) = create_opaque_rollout(
        &server,
        Some("primary-acct"),
        "PRIMARY_ACCOUNT_OPAQUE_CHECKPOINT",
    )
    .await?;
    write_account_pool_fixture(home.path());
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "primary limit reached",
                "resets_at": chrono::Utc::now().timestamp() + 3600,
            }
        })))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;
    let (resumed, _pool) = resume_with_profile(
        &server,
        test_codex().without_auth(),
        home,
        rollout_path,
        "primary-acct",
    )
    .await?;

    start_text_turn(
        &resumed.codex,
        "fail over without leaking the opaque checkpoint",
    )
    .await?;
    let error_message = wait_for_turn_error(&resumed.codex).await?;

    assert!(
        server
            .received_requests()
            .await
            .expect("captured failover requests")
            .iter()
            .all(|request| {
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    != Some("Bearer access-backup")
            })
    );
    let runtime_state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        resumed.home.path().join("account-runtime-state.json"),
    )?)?;
    let primary_state = runtime_state["profiles"]
        .as_array()
        .and_then(|profiles| {
            profiles
                .iter()
                .find(|profile| profile["profile_id"] == "primary-acct")
        })
        .expect("persisted primary runtime state");
    assert!(primary_state["exhausted_until"].is_string());
    assert_eq!(runtime_state["active_profile_id"], "backup-acct");
    assert_migration_error(&error_message, "primary-acct");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_history_owner_compact_migrates_before_a_target_turn() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let (home, rollout_path) = create_opaque_rollout(
        &server,
        Some("primary-acct"),
        "PRIMARY_OWNER_OPAQUE_CHECKPOINT",
    )
    .await?;
    write_account_pool_fixture(home.path());
    let sampling = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("owner-local-compact"),
                ev_assistant_message("owner-portable-summary", "portable owner summary"),
                ev_completed("owner-local-compact"),
            ]),
            sse(vec![
                ev_response_created("target-after-migration"),
                ev_assistant_message("target-after-migration-message", "target completed"),
                ev_completed("target-after-migration"),
            ]),
        ],
    )
    .await;

    let (owner, pool) = resume_with_profile(
        &server,
        test_codex().without_auth(),
        Arc::clone(&home),
        rollout_path.clone(),
        "primary-acct",
    )
    .await?;

    owner.codex.submit(Op::Compact).await?;
    wait_for_event(&owner.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let compact_requests = sampling.requests();
    assert_eq!(compact_requests.len(), 1);
    assert_eq!(
        compact_requests[0].header("authorization"),
        Some("Bearer access-primary".to_string()),
    );
    assert!(
        compact_requests[0]
            .body_json()
            .to_string()
            .contains("PRIMARY_OWNER_OPAQUE_CHECKPOINT")
    );
    let (summary_owner, _) = latest_compaction_summary_execution_provenance(&rollout_path)?;
    assert_eq!(summary_owner.as_deref(), Some("primary-acct"));

    owner.codex.shutdown_and_wait().await?;
    pool.activate(
        &codex_login::AccountProfileId::new("backup-acct")?,
        /*force*/ false,
    )
    .await?;
    let mut target_builder = test_codex().without_auth();
    let target = target_builder
        .resume_with_auto_env(&server, home, rollout_path)
        .await?;
    target.submit_turn("continue after owner migration").await?;

    let requests = sampling.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer access-backup".to_string()),
    );
    let target_input = requests[1].body_json().to_string();
    assert!(target_input.contains("portable owner summary"));
    assert!(!target_input.contains("PRIMARY_OWNER_OPAQUE_CHECKPOINT"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_history_owner_compact_failure_preserves_live_and_persisted_checkpoint()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let (home, rollout_path) = create_opaque_rollout(
        &server,
        Some("primary-acct"),
        "RETRYABLE_PRIMARY_OPAQUE_CHECKPOINT",
    )
    .await?;
    write_account_pool_fixture(home.path());
    let response_index = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(move |_request: &wiremock::Request| {
            if response_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(500).set_body_string("compaction failed")
            } else {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![
                        ev_response_created("owner-compact-retry"),
                        ev_assistant_message(
                            "owner-compact-retry-summary",
                            "portable summary after retry",
                        ),
                        ev_completed("owner-compact-retry"),
                    ]))
            }
        })
        .expect(/*requests*/ 2)
        .mount(&server)
        .await;

    let owner_builder = test_codex().without_auth().with_config(|config| {
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    });
    let (owner, _pool) = resume_with_profile(
        &server,
        owner_builder,
        home,
        rollout_path.clone(),
        "primary-acct",
    )
    .await?;
    let compactions_before_failure = persisted_compactions(&rollout_path)?;

    owner.codex.submit(Op::Compact).await?;
    let _failure_message = wait_for_turn_error(&owner.codex).await?;
    assert_eq!(
        persisted_compactions(&rollout_path)?,
        compactions_before_failure,
        "failed compaction must not persist a replacement checkpoint",
    );

    owner.codex.submit(Op::Compact).await?;
    wait_for_event(&owner.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let requests = server
        .received_requests()
        .await
        .expect("captured owner compaction attempts")
        .into_iter()
        .filter(|request| {
            request.url.path() == "/v1/responses"
                && request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-primary")
        })
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    let request_bodies = requests
        .iter()
        .map(decoded_request_body)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        request_bodies.iter().all(|body| body
            .to_string()
            .contains("RETRYABLE_PRIMARY_OPAQUE_CHECKPOINT")),
        "owner retry lost the original opaque checkpoint: {request_bodies:#?}",
    );
    assert_eq!(
        latest_compaction_summary_execution_provenance(&rollout_path)?.0,
        Some("primary-acct".to_string()),
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opaque_history_manual_compact_wrong_owner_surfaces_migration_error() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let (home, rollout_path) = create_opaque_rollout(
        &server,
        Some("primary-acct"),
        "WRONG_OWNER_MANUAL_COMPACT_OPAQUE",
    )
    .await?;
    write_account_pool_fixture(home.path());
    let (target, _pool) = resume_with_profile(
        &server,
        test_codex().without_auth(),
        home,
        rollout_path.clone(),
        "backup-acct",
    )
    .await?;
    let compactions_before = persisted_compactions(&rollout_path)?;
    let requests_before = response_request_count(&server).await;

    target.codex.submit(Op::Compact).await?;
    let error_message = wait_for_turn_error(&target.codex).await?;

    assert_eq!(response_request_count(&server).await, requests_before);
    assert_eq!(persisted_compactions(&rollout_path)?, compactions_before);
    assert_migration_error(&error_message, "primary-acct");
    Ok(())
}
