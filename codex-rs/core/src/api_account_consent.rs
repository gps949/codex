//! Revocable permission for the first request of a captured paid fallback.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_login::ApiAccountSelection;
use codex_login::ApiAccountStore;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::openai_models::InputModality;

use super::ApiExecutionTarget;
use super::ApiTurnPolicy;
use crate::session::turn_context::TurnContext;

/// Once a request starts, its captured destination must survive tool continuations and compaction.
#[derive(Default)]
pub(super) struct PendingApiFallback {
    started: AtomicBool,
}

pub(super) async fn fallback_is_authorized(
    turn: &TurnContext,
    target: &ApiExecutionTarget,
) -> bool {
    let Some(policy) = turn.extension_data.get::<ApiTurnPolicy>() else {
        return false;
    };
    let Some(authorized) = &policy.fallback_authorization else {
        return false;
    };
    let config = &turn.config;
    let store = ApiAccountStore::new(
        config.codex_home.to_path_buf(),
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let state = loop {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        match store.try_load() {
            Ok(Some(state)) => break state,
            Ok(None) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(_) => return false,
        }
    };
    state.selection == ApiAccountSelection::Subscription
        && state.fallback.enabled
        && state.fallback == *authorized
        && state.accounts.iter().any(|account| {
            account.id == target.profile_id
                && !account.disabled
                && Some(&account.base_url) == target.provider.info().base_url.as_ref()
                && account.model == target.model_info.slug
                && Some(account.context_window) == target.model_info.context_window
                && account.images
                    == target
                        .model_info
                        .input_modalities
                        .contains(&InputModality::Image)
        })
}

/// Changes take effect during waiting, without waiting out the previously authorized deadline.
pub(super) async fn wait_for_revocation(turn: &TurnContext, target: &ApiExecutionTarget) {
    loop {
        if !fallback_is_authorized(turn, target).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Called at the outbound request boundary. Manual API selection needs no fallback permission.
/// The finite lock polling is cancellation-safe when the caller drops this future.
pub(crate) async fn authorize_api_request(turn: &TurnContext) -> Result<()> {
    let Some(consent) = turn.extension_data.get::<PendingApiFallback>() else {
        return Ok(());
    };
    if consent.started.load(Ordering::Acquire) {
        return Ok(());
    }
    let target = turn.extension_data.get::<ApiExecutionTarget>();
    if let Some(target) = target
        && fallback_is_authorized(turn, &target).await
    {
        consent.started.store(true, Ordering::Release);
        return Ok(());
    }
    Err(CodexErr::InvalidRequest(
        "The API fallback was changed or disabled. This turn stopped before starting paid API usage."
            .into(),
    ))
}
