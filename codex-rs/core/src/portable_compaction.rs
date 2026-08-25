use crate::execution_auth::ExecutionAuth;

/// Returns true when compaction output must remain transferable between execution accounts.
///
/// OpenAI remote compaction may contain account-scoped encrypted state. Once a logical thread can
/// move between ChatGPT identities, making that blob the only surviving representation of old
/// context would make later failover impossible. Native multi-account sessions therefore create
/// portable local/plaintext compaction checkpoints instead.
pub(crate) fn requires_portable_compaction(execution_auth: &ExecutionAuth) -> bool {
    execution_auth.multi_account_enabled()
}
