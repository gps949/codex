//! Preserves host policy failure semantics without turning policy outages into sign-outs.
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PrimaryLoginPolicyFailure {
    #[error("Remote Control is disabled by the selected host account's requirements")]
    RemoteDisabled,
    #[error("Selected host authentication is disallowed by current requirements")]
    AuthenticationDenied,
}

impl PrimaryLoginPolicyFailure {
    pub fn into_io_error(self) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, self)
    }
}

pub(super) fn classified_policy_error(error: io::Error) -> io::Error {
    let permanent = error.get_ref().is_some_and(
        <dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static>::is::<
            PrimaryLoginPolicyFailure,
        >,
    );
    io::Error::other(crate::RefreshTokenError::Policy(if permanent {
        codex_http_client::NetworkPolicyDenied::Destination
    } else {
        codex_http_client::NetworkPolicyDenied::Unavailable
    }))
}
