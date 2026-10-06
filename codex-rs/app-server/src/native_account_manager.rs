//! Model-free native controls scoped to their opening connection and exact turn.

use crate::account_management::AccountManager;
use crate::native_account_capabilities::NativeAccountCapabilities;
use crate::native_account_capabilities::NativeAccountLanguage;
use crate::native_account_capabilities::QuestionObservation;
use crate::native_account_view::FrozenAccountInventory;
use crate::native_account_view::MenuAction;
use crate::native_account_view::MenuChoice;
use crate::native_account_view::MenuQuestion;
use crate::native_account_view::NativeMenuEntry;
use crate::native_account_view::actions::MenuAnswer;
use crate::native_account_view::actions::NativeMenuSession;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::ServerRequestResolvedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ToolRequestUserInputOption;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputQuestion;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[path = "native_account_manager_worker.rs"]
mod worker;

pub(crate) struct NativeMenuTarget {
    pub(crate) thread_id: ThreadId,
    pub(crate) turn_id: String,
}

#[derive(Clone, Copy)]
pub(crate) struct NativeMenuOptions {
    pub(crate) entry: NativeMenuEntry,
    pub(crate) language: NativeAccountLanguage,
}

struct NativeMenuInput {
    user: ThreadItem,
    inventory: FrozenAccountInventory,
    thread: Arc<codex_core::CodexThread>,
}

#[derive(Default)]
pub(crate) struct NativeAccountManager {
    state: Mutex<MenuState>,
    limits: MenuLimits,
}

#[derive(Default)]
struct MenuState {
    connections: HashMap<ConnectionId, NativeAccountCapabilities>,
    menus: HashMap<ThreadId, ActiveMenu>,
}

#[derive(Clone)]
struct ActiveMenu {
    owner: ConnectionId,
    thread_id: ThreadId,
    turn_id: String,
    cancellation: CancellationToken,
    finished: watch::Receiver<Option<Result<(), String>>>,
    kind: MenuKind,
}

#[derive(Clone)]
enum MenuKind {
    Synthetic(Arc<codex_core::CodexThread>),
    Attached(Arc<codex_core::CodexThread>),
}

struct MenuLimits {
    question: Duration,
    session: Duration,
    delivery: Duration,
    finish: Duration,
}

impl Default for MenuLimits {
    fn default() -> Self {
        Self {
            question: Duration::from_secs(/*secs*/ 90),
            session: Duration::from_secs(/*secs*/ 600),
            delivery: Duration::from_secs(/*secs*/ 2),
            finish: Duration::from_secs(/*secs*/ 35),
        }
    }
}

impl NativeAccountManager {
    pub(crate) fn register_connection(
        &self,
        owner: ConnectionId,
        capabilities: NativeAccountCapabilities,
    ) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .insert(owner, capabilities);
    }

    pub(crate) fn describe(&self, owner: ConnectionId, language: NativeAccountLanguage) -> String {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .get(&owner)
            .map(|capabilities| capabilities.describe(language))
            .unwrap_or_else(|| {
                language
                    .text(
                        "Connection capabilities are unavailable; reconnect and retry.",
                        "无法读取此连接能力；请重新连接后重试。",
                    )
                    .into()
            })
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        request_id: &ConnectionRequestId,
        thread: Arc<codex_core::CodexThread>,
        params: &TurnStartParams,
        manager: Arc<AccountManager>,
        outgoing: Arc<OutgoingMessageSender>,
        options: NativeMenuOptions,
    ) -> Result<(), String> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|_| "Invalid thread ID".to_string())?;
        self.cancel_thread_for_owner(request_id.connection_id, thread_id)
            .await?;
        let inventory = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), manager.inventory())
            .await
            .map_err(|_| "Account inventory timed out. Reopen the menu to retry.".to_string())?
            .map_err(|_| {
                "Account inventory could not be loaded. Check host account settings.".to_string()
            })?;
        let inventory = FrozenAccountInventory::from_inventory(inventory);
        let user = ThreadItem::UserMessage {
            id: Uuid::now_v7().to_string(),
            client_id: params.client_user_message_id.as_deref().map(|id| {
                crate::native_account_capabilities::bounded_text(id, /*max_chars*/ 128)
            }),
            content: vec![codex_app_server_protocol::UserInput::Text {
                text: match params.input.first() {
                    Some(codex_app_server_protocol::UserInput::Text { text, .. }) => {
                        crate::native_account_capabilities::bounded_text(
                            text, /*max_chars*/ 256,
                        )
                    }
                    _ => "/account manage".into(),
                },
                text_elements: vec![],
            }],
        };
        let _ordering = outgoing
            .native_account_ordering
            .lock_thread(thread_id)
            .await;
        if thread.active_turn_environment_selections().await.is_some() {
            return Err(
                "A turn is running. Open the Status panel or wait for it to finish.".into(),
            );
        }
        self.start_inventory(
            request_id,
            thread_id,
            NativeMenuInput {
                user,
                inventory,
                thread,
            },
            manager,
            outgoing,
            options,
        )
        .await
    }

    async fn start_inventory(
        self: &Arc<Self>,
        request_id: &ConnectionRequestId,
        thread_id: ThreadId,
        input: NativeMenuInput,
        manager: Arc<AccountManager>,
        outgoing: Arc<OutgoingMessageSender>,
        options: NativeMenuOptions,
    ) -> Result<(), String> {
        let owner = request_id.connection_id;
        let (finished_tx, finished) = watch::channel(/*init*/ None);
        let menu = ActiveMenu {
            owner,
            thread_id,
            turn_id: Uuid::now_v7().to_string(),
            cancellation: CancellationToken::new(),
            finished,
            kind: MenuKind::Synthetic(Arc::clone(&input.thread)),
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.connections.contains_key(&owner) {
                return Err("The menu connection is no longer available.".into());
            }
            if state.menus.contains_key(&thread_id) || state.menus.len() >= 32 {
                return Err("Another account menu is active. Close it and retry.".into());
            }
            state.menus.insert(thread_id, menu.clone());
        }
        let turn = Turn {
            id: menu.turn_id.clone(),
            items: vec![],
            items_view: TurnItemsView::NotLoaded,
            status: TurnStatus::InProgress,
            error: None,
            started_at: Some(chrono::Utc::now().timestamp()),
            completed_at: None,
            duration_ms: None,
        };
        outgoing.record_request_turn_id(request_id, &turn.id).await;
        let delivered = tokio::select! {
            biased;
            _ = menu.cancellation.cancelled() => false,
            result = tokio::time::timeout(self.limits.delivery, outgoing.send_response(request_id.clone(), TurnStartResponse { turn: turn.clone() })) => result.is_ok(),
        };
        if !delivered {
            let result = Err("Account menu response could not be delivered.".into());
            finished_tx.send_replace(Some(result.clone()));
            self.remove_menu(thread_id, &menu.turn_id);
            return result;
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            let result = coordinator
                .run(&menu, input, manager, outgoing.as_ref(), options, turn)
                .await;
            finished_tx.send_replace(Some(result.clone()));
            if result.is_ok() {
                coordinator.remove_menu(thread_id, &menu.turn_id);
            }
        });
        Ok(())
    }

    /// Signals a real turn's attached controls synchronously, without blocking its completion.
    pub(crate) fn cancel_attached_turn(&self, thread_id: ThreadId, turn_id: &str) {
        if let Some(menu) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .menus
            .get(&thread_id)
            && menu.turn_id == turn_id
            && matches!(menu.kind, MenuKind::Attached(_))
        {
            menu.cancellation.cancel();
        }
    }

    pub(crate) async fn cancel_thread(&self, thread_id: ThreadId) -> Result<(), String> {
        let menu = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .menus
            .get(&thread_id)
            .cloned();
        let Some(menu) = menu else {
            return Ok(());
        };
        menu.cancellation.cancel();
        let result = self.wait_finished(menu.clone()).await;
        // A finished failed delivery cannot later emit a completion into a new real turn.
        if menu.finished.borrow().is_some() {
            self.remove_menu(thread_id, &menu.turn_id);
        }
        result
    }

    pub(crate) async fn try_interrupt(
        &self,
        owner: ConnectionId,
        thread_id: ThreadId,
        turn_id: &str,
    ) -> Result<bool, String> {
        let attached = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .menus
            .get(&thread_id)
            .is_some_and(|menu| {
                menu.owner == owner
                    && matches!(menu.kind, MenuKind::Attached(_))
                    && (turn_id.is_empty() || menu.turn_id == turn_id)
            });
        if attached {
            self.cancel_thread(thread_id).await?;
            return Ok(false);
        }
        let matches = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .menus
            .get(&thread_id)
            .is_some_and(|menu| {
                menu.owner == owner
                    && matches!(menu.kind, MenuKind::Synthetic(_))
                    && (turn_id.is_empty() || menu.turn_id == turn_id)
            });
        if !matches {
            return Ok(false);
        }
        self.cancel_thread(thread_id).await?;
        Ok(true)
    }

    pub(crate) async fn cancel_thread_for_owner(
        &self,
        owner: ConnectionId,
        thread_id: ThreadId,
    ) -> Result<(), String> {
        let owned = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .menus
            .get(&thread_id)
            .is_some_and(|menu| menu.owner == owner);
        if owned {
            self.cancel_thread(thread_id).await
        } else {
            Ok(())
        }
    }

    pub(crate) async fn unregister(&self, owner: ConnectionId) -> Result<(), String> {
        let menus = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.connections.remove(&owner);
            state
                .menus
                .iter()
                .filter(|(_, menu)| menu.owner == owner)
                .map(|(id, menu)| (*id, menu.clone()))
                .collect::<Vec<_>>()
        };
        self.close_menus(menus).await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let menus = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.connections.clear();
            state
                .menus
                .iter()
                .map(|(id, menu)| (*id, menu.clone()))
                .collect()
        };
        self.close_menus(menus).await
    }

    async fn close_menus(&self, menus: Vec<(ThreadId, ActiveMenu)>) -> Result<(), String> {
        for (_, menu) in &menus {
            menu.cancellation.cancel();
        }
        let outcomes = futures::future::join_all(menus.into_iter().map(|(id, menu)| async move {
            let result = self.wait_finished(menu.clone()).await;
            if menu.finished.borrow().is_some() {
                self.remove_menu(id, &menu.turn_id);
            }
            result
        }))
        .await;
        outcomes
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map(|_| ())
    }

    async fn wait_finished(&self, mut menu: ActiveMenu) -> Result<(), String> {
        tokio::time::timeout(self.limits.finish, async move {
            loop {
                if let Some(result) = menu.finished.borrow().clone() {
                    return result;
                }
                menu.finished
                    .changed()
                    .await
                    .map_err(|_| "Account menu ended without a completion.".to_string())?;
            }
        })
        .await
        .map_err(|_| "Account menu is still closing. Retry after it finishes.".to_string())?
    }

    fn remove_menu(&self, thread_id: ThreadId, turn_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .menus
            .get(&thread_id)
            .is_some_and(|menu| menu.turn_id == turn_id)
        {
            state.menus.remove(&thread_id);
        }
    }

    fn observe(&self, owner: ConnectionId, observation: QuestionObservation) {
        if let Some(connection) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .connections
            .get_mut(&owner)
        {
            connection.question = observation;
        }
    }
}

#[cfg(test)]
#[path = "native_account_manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_account_thread_routing_tests.rs"]
mod thread_routing_tests;
