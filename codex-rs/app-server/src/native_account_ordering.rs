//! Orders synthetic controls against real turn events, including autonomous queues.

use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use tokio::sync::OwnedMutexGuard;

#[derive(Default)]
pub(crate) struct NativeAccountOrdering {
    threads: Mutex<HashMap<ThreadId, Weak<tokio::sync::Mutex<()>>>>,
}

impl NativeAccountOrdering {
    pub(crate) async fn lock_thread(&self, thread_id: ThreadId) -> OwnedMutexGuard<()> {
        let gate = {
            let mut threads = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            threads.retain(|_, gate| gate.strong_count() > 0);
            match threads.get(&thread_id).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(tokio::sync::Mutex::new(()));
                    threads.insert(thread_id, Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.lock_owned().await
    }
}
