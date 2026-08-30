use codex_login::AuthManager;
use codex_login::CodexAuth;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn request_auth_retains_the_bound_manager() -> std::io::Result<()> {
    let manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("bound-token"));
    let binding =
        ExecutionRequestAuth::new(/*profile_id*/ None, /*generation*/ 7, manager);

    assert_eq!(
        binding
            .auth_manager()
            .auth_cached()
            .expect("bound auth snapshot")
            .get_token()?,
        "bound-token",
    );
    Ok(())
}
