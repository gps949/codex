use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::CodexErrorInfo;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn non_account_failure_directive_is_stable() {
    assert!(matches!(
        SamplingFailoverDirective::NotHandled,
        SamplingFailoverDirective::NotHandled
    ));
}

#[test]
fn pool_unavailable_maps_to_usage_limit_exceeded() {
    // Empty ExecutionAuth (no pool) still produces UsageLimitReached so clients get the
    // cooldown-oriented protocol error instead of BadRequest from UnsupportedOperation.
    let execution_auth = ExecutionAuth::shared(AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("test"),
    ));
    let err = pool_unavailable_error(&execution_auth);
    assert!(matches!(
        err.details(),
        CodexErrorDetails::UsageLimitReached(_)
    ));
    assert_eq!(
        err.to_codex_protocol_error(),
        CodexErrorInfo::UsageLimitExceeded
    );
}
