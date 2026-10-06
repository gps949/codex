use super::tests::answer;
use super::tests::launch;
use super::tests::question;
use super::tests::setup;
use super::*;
use codex_protocol::protocol::ModelSelectionIntent;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn phone_auto_confirmation_changes_only_the_opened_threads_pin() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    let thread = launch(&coordinator, &outgoing, thread_id).await;
    let before = thread.thread_settings_snapshot().await;
    let policy_before = thread
        .config()
        .await
        .model_routing_snapshot()
        .await
        .unwrap();
    thread
        .update_thread_settings(codex_protocol::protocol::ThreadSettingsOverrides {
            model_selection_intent: Some(ModelSelectionIntent::Explicit),
            ..Default::default()
        })
        .await
        .unwrap();
    let (request, params) = question(&mut messages).await;
    answer(&outgoing, request, &params, "Model selection").await;
    let (request, params) = question(&mut messages).await;
    answer(&outgoing, request, &params, "Use auto for this thread").await;
    let (request, params) = question(&mut messages).await;
    assert_eq!(params.questions[0].question, "Return this thread to auto?");
    assert_eq!(
        thread
            .thread_settings_snapshot()
            .await
            .model_selection_intent,
        Some(ModelSelectionIntent::Explicit)
    );
    answer(&outgoing, request, &params, "Confirm").await;
    let (request, params) = question(&mut messages).await;
    assert_eq!(params.questions[0].question, "Operation result");
    let mut expected = before;
    expected.model_selection_intent = Some(ModelSelectionIntent::Automatic);
    assert_eq!(thread.thread_settings_snapshot().await, expected);
    assert_eq!(
        thread
            .config()
            .await
            .model_routing_snapshot()
            .await
            .unwrap(),
        policy_before
    );
    assert!(thread.active_turn_environment_selections().await.is_none());
    answer(&outgoing, request, &params, "Close").await;
    coordinator.cancel_thread(thread_id).await.unwrap();
}

#[tokio::test]
async fn closed_thread_menu_scope_rejects_late_model_mutation() {
    let (coordinator, outgoing, mut messages) = setup().await;
    let thread_id = ThreadId::new();
    let thread = launch(&coordinator, &outgoing, thread_id).await;
    question(&mut messages).await;
    thread
        .update_thread_settings(codex_protocol::protocol::ThreadSettingsOverrides {
            model_selection_intent: Some(ModelSelectionIntent::Explicit),
            ..Default::default()
        })
        .await
        .unwrap();
    let before = thread.thread_settings_snapshot().await;
    let cancellation = coordinator
        .state
        .lock()
        .unwrap()
        .menus
        .get(&thread_id)
        .unwrap()
        .cancellation
        .child_token();
    let context = crate::account_management::AccountOperationContext::ThreadMenu {
        cancellation: cancellation.clone(),
        thread: Arc::clone(&thread),
    };
    let manager = AccountManager::new((*thread.config().await).clone());
    assert!(context.ensure_current().await.is_ok());
    cancellation.cancel();
    assert!(context.ensure_current().await.is_err());
    let error = manager
        .execute_with_context(
            crate::account_management::AccountManagerOperation::ThreadModelAutomatic,
            &context,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Account menu was closed before this operation was submitted"
    );
    assert_eq!(thread.thread_settings_snapshot().await, before);
    assert!(
        manager
            .execute(crate::account_management::AccountManagerOperation::ThreadModelAutomatic)
            .await
            .is_err()
    );
    coordinator.cancel_thread(thread_id).await.unwrap();
}
