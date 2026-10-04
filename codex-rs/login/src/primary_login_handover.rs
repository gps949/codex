//! One-time Remote continuation attached to an explicit, owner-bound host selection.

use crate::CodexAuth;
use crate::primary_login::tokens_owner_hash;
use serde::Deserialize;
use serde::Serialize;
use std::io;

const HANDOVER_LIFETIME_SECONDS: i64 = 120;

/// A short-lived user selection intent; it never transfers enrollment or device permissions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryLoginHandover {
    pub id: String,
    pub expires_at: i64,
    pub target_owner_hash: String,
}

impl PrimaryLoginHandover {
    pub(crate) fn for_auth(auth: &CodexAuth) -> io::Result<Self> {
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            expires_at: chrono::Utc::now().timestamp() + HANDOVER_LIFETIME_SECONDS,
            target_owner_hash: tokens_owner_hash(&auth.get_token_data()?)?,
        })
    }

    pub fn matches_auth(&self, auth: &CodexAuth) -> bool {
        auth.get_token_data()
            .ok()
            .and_then(|tokens| tokens_owner_hash(&tokens).ok())
            .is_some_and(|owner| owner == self.target_owner_hash)
    }

    pub fn is_current(&self) -> bool {
        self.expires_at
            .checked_sub(chrono::Utc::now().timestamp())
            .is_some_and(|remaining| (0..=HANDOVER_LIFETIME_SECONDS).contains(&remaining))
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if uuid::Uuid::parse_str(&self.id).is_err()
            || self.target_owner_hash.len() != 64
            || !self
                .target_owner_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "primary-login handover is invalid",
            ));
        }
        Ok(())
    }
}

/// Observes explicit selections before old credentials retire, then acknowledges local application.
/// Implementations must bind continuation to the supplied revision/owner and must never revive
/// disabled Remote access, unexpected relogins or signed-out sources.
pub trait PrimaryLoginTransitionObserver: Send + Sync {
    fn selection_unavailable(&self);
    fn before_selection(&self, state: &crate::PrimaryLoginState);
    fn after_selection(&self, state: &crate::PrimaryLoginState, auth: Option<&CodexAuth>);
}
