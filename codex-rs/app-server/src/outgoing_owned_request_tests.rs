use super::*;
use codex_app_server_protocol::McpServerElicitationRequest;
use codex_app_server_protocol::McpServerElicitationRequestParams;
use codex_app_server_protocol::ToolRequestUserInputParams;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::future::Future;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use tracing::instrument::WithSubscriber;

fn question(thread_id: ThreadId) -> ServerRequestPayload {
    ServerRequestPayload::ToolRequestUserInput(ToolRequestUserInputParams {
        thread_id: thread_id.to_string(),
        turn_id: "manager-turn".into(),
        item_id: "manager-question".into(),
        questions: vec![],
        is_blocking: true,
        auto_resolution_ms: None,
    })
}

async fn received_request(
    messages: &mut mpsc::Receiver<OutgoingEnvelope>,
) -> (ConnectionId, ServerRequest) {
    let Some(OutgoingEnvelope::ToConnection {
        connection_id,
        message: OutgoingMessage::Request(request),
        write_complete_tx: None,
    }) = messages.recv().await
    else {
        panic!("expected a targeted server request");
    };
    (connection_id, request)
}

#[tokio::test]
async fn owned_response_and_error_require_the_owner_without_consuming_the_callback() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let payload = question(thread_id);
    let (id, mut response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            payload.clone(),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert_eq!(
        received_request(&mut messages).await,
        (owner, payload.request_with_id(id.clone()))
    );
    let answer = json!({"answers": {"account": {"answers": ["Show overview"]}}});
    outgoing
        .notify_client_response(ConnectionId(2), id.clone(), answer.clone())
        .await;
    outgoing
        .notify_client_error(ConnectionId(2), id.clone(), internal_error("wrong owner"))
        .await;
    assert_eq!(
        response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert!(
        outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&id)
    );
    outgoing
        .notify_client_response(owner, id.clone(), answer.clone())
        .await;
    assert_eq!(response.await.unwrap(), Ok(answer));
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());

    let (id, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let error = internal_error("owner dismissed the question");
    outgoing.notify_client_error(owner, id, error.clone()).await;
    assert_eq!(response.await.unwrap(), Err(error));
}

#[tokio::test]
async fn owned_requests_are_not_replayed_but_shared_requests_remain_replayable_and_answerable() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let (owned_id, owned_response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let payload = question(thread_id);
    let (shared_id, shared_response) = outgoing
        .send_request_to_connections(Some(&[owner]), payload.clone(), Some(thread_id))
        .await;
    received_request(&mut messages).await;
    let shared_request = payload.request_with_id(shared_id.clone());
    assert_eq!(
        outgoing.pending_requests_for_thread(thread_id).await,
        vec![shared_request.clone()]
    );
    outgoing
        .replay_requests_to_connection_for_thread(ConnectionId(2), thread_id)
        .await;
    assert_eq!(
        received_request(&mut messages).await,
        (ConnectionId(2), shared_request)
    );
    assert!(messages.try_recv().is_err());
    let answer = json!({"answers": {}});
    outgoing
        .notify_client_response(ConnectionId(2), shared_id, answer.clone())
        .await;
    assert_eq!(shared_response.await.unwrap(), Ok(answer.clone()));
    outgoing
        .notify_client_response(owner, owned_id, answer.clone())
        .await;
    assert_eq!(owned_response.await.unwrap(), Ok(answer));
}

#[tokio::test]
async fn disconnect_cancels_only_that_owners_requests_and_removes_its_live_scope() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let thread_id = ThreadId::new();
    outgoing
        .register_connection_owned_scope(ConnectionId(1))
        .await;
    outgoing
        .register_connection_owned_scope(ConnectionId(2))
        .await;
    let (first_id, first_response) = outgoing
        .send_connection_owned_request_with_cancellation(
            ConnectionId(1),
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let (second_id, mut second_response) = outgoing
        .send_connection_owned_request_with_cancellation(
            ConnectionId(2),
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let (shared_id, mut shared_response) = outgoing
        .send_request_to_connections(
            Some(&[ConnectionId(1)]),
            question(thread_id),
            Some(thread_id),
        )
        .await;
    received_request(&mut messages).await;

    outgoing
        .disconnect_connection_owned_scope(ConnectionId(1))
        .await;
    assert!(first_response.await.is_err());
    assert_eq!(
        second_response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert_eq!(
        shared_response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert!(
        !outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&first_id)
    );
    assert!(
        !outgoing
            .connection_owned_scopes
            .lock()
            .await
            .contains_key(&ConnectionId(1))
    );
    assert!(
        outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&second_id)
    );

    outgoing.connection_closed(ConnectionId(2)).await;
    assert!(second_response.await.is_err());
    assert!(outgoing.connection_owned_scopes.lock().await.is_empty());
    let answer = json!({"answers": {}});
    outgoing
        .notify_client_response(ConnectionId(3), shared_id, answer.clone())
        .await;
    assert_eq!(shared_response.await.unwrap(), Ok(answer));
}

#[tokio::test]
async fn disconnect_before_callback_insertion_leaves_no_owned_callback_or_message() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let mut registration =
        std::pin::pin!(outgoing.send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new()
        ));
    let mut disconnect = std::pin::pin!(outgoing.disconnect_connection_owned_scope(owner));
    {
        let _callbacks = outgoing.request_id_to_callback.lock().await;
        let mut context = Context::from_waker(Waker::noop());
        assert!(registration.as_mut().poll(&mut context).is_pending());
        assert!(disconnect.as_mut().poll(&mut context).is_pending());
        assert!(
            outgoing
                .connection_owned_scopes
                .try_lock()
                .unwrap()
                .is_empty()
        );
    }
    let ((_, response), ()) = tokio::join!(registration, disconnect);
    assert!(response.await.is_err());
    assert!(messages.try_recv().is_err());
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
}

#[tokio::test]
async fn disconnect_after_insertion_cancels_owned_send_while_queue_is_full() {
    let (sender, mut messages) = mpsc::channel(1);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let (shared_id, shared_response) = outgoing
        .send_request_to_connections(Some(&[owner]), question(thread_id), Some(thread_id))
        .await;
    let mut registration =
        std::pin::pin!(outgoing.send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new()
        ));
    let mut context = Context::from_waker(Waker::noop());
    assert!(registration.as_mut().poll(&mut context).is_pending());
    assert_eq!(outgoing.request_id_to_callback.lock().await.len(), 2);
    // Registering an initialized connection twice must preserve its existing cancellation scope.
    outgoing.register_connection_owned_scope(owner).await;
    outgoing.disconnect_connection_owned_scope(owner).await;
    let Poll::Ready((_, response)) = registration.as_mut().poll(&mut context) else {
        panic!("disconnect must wake the owned send without waiting for queue capacity");
    };
    assert!(response.await.is_err());
    assert_eq!(
        outgoing
            .request_id_to_callback
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![shared_id.clone()]
    );
    let (_, queued) = received_request(&mut messages).await;
    assert_eq!(queued.id(), &shared_id);
    assert!(messages.try_recv().is_err());
    let answer = json!({"answers": {}});
    outgoing
        .notify_client_response(ConnectionId(2), shared_id, answer.clone())
        .await;
    assert_eq!(shared_response.await.unwrap(), Ok(answer));
}

#[tokio::test]
async fn missing_or_disconnected_owner_never_gets_a_callback() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    let thread_id = ThreadId::new();
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(response.await.is_err());
    outgoing.register_connection_owned_scope(owner).await;
    outgoing.disconnect_connection_owned_scope(owner).await;
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(response.await.is_err());
    assert!(messages.try_recv().is_err());
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
}

#[tokio::test]
async fn failed_owned_delivery_removes_its_callback() {
    let (sender, messages) = mpsc::channel(1);
    drop(messages);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(response.await.is_err());
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
}

#[tokio::test]
async fn ordinary_owned_registration_does_not_enable_user_verification() {
    let (sender, mut messages) = mpsc::channel(1);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let payload =
        ServerRequestPayload::McpServerElicitationRequest(McpServerElicitationRequestParams {
            thread_id: thread_id.to_string(),
            turn_id: Some("verification-turn".into()),
            server_name: "plugin-service".into(),
            request: McpServerElicitationRequest::UserVerification {
                meta: None,
                title: "Approve".into(),
                description: String::new(),
                challenge: "AQID".into(),
            },
        });
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            payload,
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(response.await.is_err());
    assert!(messages.try_recv().is_err());
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
    assert!(outgoing.verification_connections.lock().await.is_empty());
}

#[tokio::test]
async fn cancel_owned_request_preserves_shared_requests_and_ignores_late_answers() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let (owned_id, owned_response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let (shared_id, mut shared_response) = outgoing
        .send_request_to_connections(Some(&[owner]), question(thread_id), Some(thread_id))
        .await;
    received_request(&mut messages).await;
    assert!(outgoing.cancel_request(&owned_id).await);
    assert!(owned_response.await.is_err());
    outgoing
        .notify_client_response(owner, owned_id.clone(), json!({"answers": {}}))
        .await;
    assert!(!outgoing.cancel_request(&owned_id).await);
    assert_eq!(
        shared_response.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    assert!(
        outgoing
            .request_id_to_callback
            .lock()
            .await
            .contains_key(&shared_id)
    );
}

#[tokio::test]
async fn owned_answers_are_not_logged_but_shared_answer_logging_is_unchanged() {
    let (sender, mut messages) = mpsc::channel(4);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    outgoing.register_connection_owned_scope(owner).await;
    let thread_id = ThreadId::new();
    let (owned_id, owned_response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    received_request(&mut messages).await;
    let (shared_id, shared_response) = outgoing
        .send_request_to_connections(Some(&[owner]), question(thread_id), Some(thread_id))
        .await;
    received_request(&mut messages).await;
    let capture = tempfile::NamedTempFile::new().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(Arc::new(capture.reopen().unwrap()))
        .finish();
    async {
        outgoing
            .notify_client_response(
                owner,
                owned_id,
                json!({"answers": {"account": {"answers": ["private-owned-answer-marker"]}}}),
            )
            .await;
        outgoing
            .notify_client_response(
                owner,
                shared_id,
                json!({"answers": {"account": {"answers": ["ordinary-shared-answer-marker"]}}}),
            )
            .await;
    }
    .with_subscriber(subscriber)
    .await;
    assert!(owned_response.await.unwrap().is_ok());
    assert!(shared_response.await.unwrap().is_ok());
    let logs = std::fs::read_to_string(capture.path()).unwrap();
    assert!(!logs.contains("private-owned-answer-marker"));
    assert!(logs.contains("ordinary-shared-answer-marker"));
}

#[tokio::test]
async fn caller_cancellation_unblocks_a_full_queue_without_disconnecting_the_owner() {
    let (sender, mut messages) = mpsc::channel(1);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    let thread_id = ThreadId::new();
    outgoing.register_connection_owned_scope(owner).await;
    let (shared_id, shared_response) = outgoing
        .send_request_to_connections(Some(&[owner]), question(thread_id), Some(thread_id))
        .await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let sending = outgoing.send_connection_owned_request_with_cancellation(
        owner,
        question(thread_id),
        thread_id,
        cancellation.clone(),
    );
    tokio::pin!(sending);
    tokio::select! {
        result = &mut sending => panic!("full queue unexpectedly accepted the owned request: {result:?}"),
        _ = async {
            while outgoing.request_id_to_callback.lock().await.len() != 2 {
                tokio::task::yield_now().await;
            }
        } => {}
    }
    cancellation.cancel();
    let (_, response) = tokio::time::timeout(std::time::Duration::from_secs(1), sending)
        .await
        .expect("caller cancellation must unblock dispatch");
    assert!(response.await.is_err());
    assert_eq!(outgoing.request_id_to_callback.lock().await.len(), 1);
    assert_eq!(received_request(&mut messages).await.1.id(), &shared_id);
    let answer = json!({"answers": {}});
    outgoing
        .notify_client_response(owner, shared_id, answer.clone())
        .await;
    assert_eq!(shared_response.await.unwrap(), Ok(answer));
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    let (_, request) = received_request(&mut messages).await;
    outgoing
        .notify_client_response(owner, request.id().clone(), json!({"answers": {}}))
        .await;
    assert!(response.await.unwrap().is_ok());
}

#[tokio::test]
async fn cancelled_request_does_not_register_and_cancelled_late_answer_is_rejected() {
    let (sender, mut messages) = mpsc::channel(2);
    let outgoing = OutgoingMessageSender::new(sender, AnalyticsEventsClient::disabled());
    let owner = ConnectionId(1);
    let thread_id = ThreadId::new();
    outgoing.register_connection_owned_scope(owner).await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    let (_, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            cancellation,
        )
        .await;
    assert!(response.await.is_err());
    assert!(messages.try_recv().is_err());
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (id, response) = outgoing
        .send_connection_owned_request_with_cancellation(
            owner,
            question(thread_id),
            thread_id,
            cancellation.clone(),
        )
        .await;
    received_request(&mut messages).await;
    cancellation.cancel();
    outgoing
        .notify_client_response(owner, id, json!({"answers": {}}))
        .await;
    assert!(response.await.is_err());
    assert!(outgoing.request_id_to_callback.lock().await.is_empty());
}
