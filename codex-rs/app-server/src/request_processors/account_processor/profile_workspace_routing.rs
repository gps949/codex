//! Per-profile discovery for standby requests; foreground routing state stays separate.

use super::AccountRequestProcessor;
use super::workspace_routing::CachedWorkspaceRouting;
use super::workspace_routing::WorkspaceRoutingFetches;
use codex_login::AccountProfileId;
use codex_login::AuthManager;
use codex_login::WorkspaceRouting;
use codex_login::WorkspaceRoutingRequest;
use codex_login::WorkspaceRoutingResolver;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::Weak;
use tokio::sync::Mutex;

#[derive(Default)]
pub(super) struct ProfileRoutingOwners {
    processor: OnceLock<Weak<AccountRequestProcessor>>,
    profiles: Mutex<HashMap<AccountProfileId, Arc<ProfileRoutingOwner>>>,
}

impl ProfileRoutingOwners {
    pub(super) fn initialize(&self, processor: Weak<AccountRequestProcessor>) {
        let _ = self.processor.set(processor);
    }

    pub(super) async fn synchronize(&self, managers: Vec<(AccountProfileId, Arc<AuthManager>)>) {
        let Some(processor) = self.processor.get() else {
            return;
        };
        let mut profiles = self.profiles.lock().await;
        profiles.retain(|id, owner| {
            managers.iter().any(|(candidate, manager)| {
                candidate == id && owner.auth_manager.ptr_eq(&Arc::downgrade(manager))
            })
        });
        for (id, manager) in managers {
            if profiles.contains_key(&id) {
                continue;
            }
            let owner = Arc::new(ProfileRoutingOwner {
                processor: processor.clone(),
                auth_manager: Arc::downgrade(&manager),
                cache: Arc::new(Mutex::new(None)),
                fetches: Arc::new(Mutex::new(HashMap::new())),
                config_manager: processor.upgrade().map(|processor| {
                    processor
                        .config_manager
                        .for_profile_routing(Arc::clone(&manager), &processor.config)
                }),
            });
            let resolver: Arc<dyn WorkspaceRoutingResolver> = owner.clone();
            if manager.set_workspace_routing_resolver_if_unset(Arc::downgrade(&resolver)) {
                profiles.insert(id, owner);
            }
        }
    }
}

struct ProfileRoutingOwner {
    processor: Weak<AccountRequestProcessor>,
    auth_manager: Weak<AuthManager>,
    cache: Arc<Mutex<Option<CachedWorkspaceRouting>>>,
    fetches: Arc<Mutex<WorkspaceRoutingFetches>>,
    config_manager: Option<crate::config_manager::ConfigManager>,
}

impl WorkspaceRoutingResolver for ProfileRoutingOwner {
    fn maintenance_clients(
        &self,
    ) -> Pin<
        Box<
            dyn Future<Output = io::Result<Option<codex_login::WorkspaceMaintenanceClients>>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let manager = self
                .config_manager
                .as_ref()
                .ok_or_else(|| io::Error::other("standby requirements owner is unavailable"))?;
            let config = manager.load_latest_config(/*fallback_cwd*/ None).await?;
            Ok(Some(codex_login::WorkspaceMaintenanceClients {
                http_client_factory: config
                    .http_client_factory()
                    .with_network_policy(config.application_network_policy.for_current_account()),
                chatgpt_base_url: config.chatgpt_base_url,
            }))
        })
    }

    fn resolve(
        &self,
        request: WorkspaceRoutingRequest,
    ) -> Pin<Box<dyn Future<Output = io::Result<Option<WorkspaceRouting>>> + Send + '_>> {
        Box::pin(async move {
            let processor = self
                .processor
                .upgrade()
                .ok_or_else(|| io::Error::other("standby routing owner is unavailable"))?;
            let auth_manager = self
                .auth_manager
                .upgrade()
                .ok_or_else(|| io::Error::other("standby authentication owner is unavailable"))?;
            // Reuse discovery, validation, retained settings, shutdown and auth-change guards,
            // while the selected foreground workspace and its cache remain untouched.
            let scoped = AccountRequestProcessor {
                auth_manager,
                workspace_routing: Arc::clone(&self.cache),
                workspace_routing_fetches: Arc::clone(&self.fetches),
                config_manager: self
                    .config_manager
                    .clone()
                    .ok_or_else(|| io::Error::other("standby requirements owner is unavailable"))?,
                ..processor.as_ref().clone()
            };
            let provider_url = request.provider_base_url.clone();
            let retained = request.session.clone();
            let routing = WorkspaceRoutingResolver::resolve(&scoped, request).await?;
            let config = match retained {
                Some(session) => {
                    scoped
                        .config_manager
                        .load_retained_session_config(&session.config_layer_stack, &session.cwd)
                        .await?
                }
                None => {
                    scoped
                        .config_manager
                        .load_latest_config(/*fallback_cwd*/ None)
                        .await?
                }
            };
            let destination = routing
                .as_ref()
                .map_or(provider_url, |routing| routing.backend_origin.clone());
            let destination = url::Url::parse(&destination).map_err(io::Error::other)?;
            config
                .http_client_factory()
                .network_policy()
                .acquire(&destination)
                .map_err(io::Error::other)?
                .check()
                .map_err(io::Error::other)?;
            Ok(routing)
        })
    }
}
