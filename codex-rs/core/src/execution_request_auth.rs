use std::sync::Arc;

use codex_login::AccountProfileId;
use codex_login::AuthManager;

/// Immutable authentication source captured for one concrete model request.
#[derive(Clone)]
pub(crate) struct ExecutionRequestAuth {
    profile_id: Option<AccountProfileId>,
    generation: u64,
    auth_manager: Arc<AuthManager>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutionRequestIdentity {
    profile_id: Option<AccountProfileId>,
    generation: u64,
}

impl ExecutionRequestAuth {
    pub(crate) fn new(
        profile_id: Option<AccountProfileId>,
        generation: u64,
        auth_manager: Arc<AuthManager>,
    ) -> Self {
        Self {
            profile_id,
            generation,
            auth_manager,
        }
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub(crate) fn identity(&self) -> ExecutionRequestIdentity {
        ExecutionRequestIdentity {
            profile_id: self.profile_id.clone(),
            generation: self.generation,
        }
    }
}

#[cfg(test)]
#[path = "execution_request_auth_tests.rs"]
mod tests;
