//! Pins native management work to its opening cancellation scope and real turn.

use codex_core::CodexThread;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) enum AccountOperationContext {
    Independent,
    NativeMenu(CancellationToken),
    AttachedMenu {
        cancellation: CancellationToken,
        thread: Arc<CodexThread>,
        turn_id: String,
    },
}

impl AccountOperationContext {
    /// Checks immediately before a consequential backend request, after earlier awaits.
    pub(crate) async fn ensure_current(&self) -> anyhow::Result<()> {
        match self {
            Self::Independent => Ok(()),
            Self::NativeMenu(cancellation) => {
                anyhow::ensure!(
                    !cancellation.is_cancelled(),
                    "Account menu was closed before this operation was submitted"
                );
                Ok(())
            }
            Self::AttachedMenu {
                cancellation,
                thread,
                turn_id,
            } => {
                anyhow::ensure!(
                    !cancellation.is_cancelled()
                        && thread
                            .current_turn_environment_selections(turn_id)
                            .await
                            .is_some()
                        && !cancellation.is_cancelled(),
                    "The account menu's task ended before this operation was submitted"
                );
                Ok(())
            }
        }
    }
}
