//! Mobile commands reuse the account and config RPC implementations.

use super::*;
use crate::mobile_account_bridge::MobileSlashCommand;
use crate::mobile_account_status as view;
use crate::request_processors::config_processor::ConfigRequestProcessor;
use crate::request_serialization::RequestSerializationAccess;
use crate::request_serialization::RequestSerializationQueueKey;
use crate::request_serialization::RequestSerializationQueues;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigEdit;
use codex_app_server_protocol::MergeStrategy;

impl AccountRequestProcessor {
    pub(crate) async fn try_handle_mobile_slash_turn(
        &self,
        request_id: ConnectionRequestId,
        params: TurnStartParams,
        client_name: Option<&str>,
        config_processor: &ConfigRequestProcessor,
        queues: &RequestSerializationQueues,
    ) -> Result<Option<TurnStartResponse>, JSONRPCErrorError> {
        let Some(command) = mobile_slash_command(&params.input, client_name) else {
            return Ok(None);
        };
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;
        super::super::thread_input::ensure_direct_input_allowed(thread.as_ref()).await?;
        // A synthetic turn must not replace the mobile client's real running turn.
        if matches!(
            thread.agent_status().await,
            codex_protocol::protocol::AgentStatus::Running
        ) {
            return Err(invalid_request(
                "A turn is running. Open the Status panel or wait for it to finish.",
            ));
        }
        let text = match self
            .mobile_account_reply(&params, command, config_processor, queues)
            .await
        {
            Ok(text) => text,
            Err(error) => error.message,
        };
        Ok(Some(
            complete_mobile_slash_turn(&self.outgoing, &request_id, thread_id, &params, text).await,
        ))
    }

    async fn mobile_account_reply(
        &self,
        params: &TurnStartParams,
        command: MobileSlashCommand,
        config_processor: &ConfigRequestProcessor,
        queues: &RequestSerializationQueues,
    ) -> Result<String, JSONRPCErrorError> {
        let V2UserInput::Text { text, .. } = &params.input[0] else {
            unreachable!()
        };
        let text = text.trim().trim_start_matches('/').trim_start();
        let args = text
            .split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest.trim());
        let mut pool = self.get_account_pool_response().await?;
        if !pool.enabled {
            return Ok("Account pool is not configured. Add accounts on the host with `codex account add`.".into());
        }
        let (verb, value) = args
            .split_once(char::is_whitespace)
            .map_or((args, ""), |(verb, value)| (verb, value.trim()));
        let verb = verb.to_ascii_lowercase();
        if command == MobileSlashCommand::Status && !args.is_empty() {
            return Err(invalid_request(
                "Usage: /status. For accounts, use /account.",
            ));
        }
        match (command, verb.as_str()) {
            (MobileSlashCommand::Status, _) | (MobileSlashCommand::Account, "" | "list" | "show") => {
                let page = if verb == "list" && !value.is_empty() {
                    value.parse::<usize>().ok().filter(|page| *page > 0)
                        .ok_or_else(|| invalid_request("Usage: /account list <page>"))?
                } else { 1 };
                let profile_ids = if verb == "show" {
                    let profile_id = view::resolve(&pool, value).map_err(invalid_request)?.profile_id.clone();
                    vec![profile_id]
                } else {
                    view::page_accounts(&pool, page).map_err(invalid_request)?
                        .into_iter().map(|account| account.profile_id.clone()).collect()
                };
                // Reject invalid page numbers before performing any network probes.
                pool_quota::refresh(&self.load_latest_config().await, &mut pool, pool_quota::RefreshScope::Profiles(profile_ids)).await;
                if verb == "show" {
                    view::detail(&pool, value).map_err(invalid_request)
                } else {
                    view::list(&pool, page).map_err(invalid_request)
                }
            }
            (_, "use" | "auto" | "retry") => {
                let profile_id = if verb == "auto" {
                    if !value.is_empty() { return Err(invalid_request("Usage: /account auto")); }
                    None
                } else {
                    let account = view::resolve(&pool, value).map_err(invalid_request)?;
                    if verb == "use" && matches!(account.availability, codex_app_server_protocol::AccountPoolAvailability::Exhausted { .. }) {
                        return Err(invalid_request("This account is cooling down. Choose another account or wait for reset. If quota was reset externally, use /account retry <label|@selector> to probe again."));
                    }
                    Some(account.profile_id.clone())
                };
                self.use_account_pool_response(codex_app_server_protocol::AccountPoolUseParams { profile_id, force: verb == "retry" }).await?;
                pool = self.get_account_pool_response().await?;
                let label = pool.accounts.iter().find(|account| account.is_active)
                    .map(view::label).unwrap_or_else(|| "Account".into());
                Ok(format!("Selected: {label}\nApplies to subsequent requests; automatic failover remains enabled."))
            }
            (_, "settings") if value.is_empty() => {
                Ok(view::settings(&self.load_latest_config().await.account_pool))
            }
            (_, "warmup") if value.is_empty() || value == "now" || value.parse::<usize>().is_ok() => {
                let page = if value.is_empty() || value == "now" { 1 } else {
                    value.parse::<usize>().map_err(|_| invalid_request("Usage: /account warmup [page|now|on|off]"))?
                };
                view::page_accounts(&pool, page).map_err(invalid_request)?;
                let debug = self.account_pool_warmup_debug_response(
                    codex_app_server_protocol::AccountPoolWarmupDebugParams { run_now: value == "now" },
                ).await?;
                pool = self.get_account_pool_response().await?;
                let text = view::warmup_status(&pool, &debug, page).map_err(invalid_request)?;
                if value == "now" && !debug.pass_requested {
                    Ok(format!("No pass requested. Enable warmup with /account warmup on first.\n\n{text}"))
                } else { Ok(text) }
            }
            (_, "warmup" | "resume" | "wait" | "reset-credits") => {
                let (key, setting) = match verb.as_str() {
                    "warmup" | "resume" => {
                        let enabled = match value {
                            "on" => true,
                            "off" => false,
                            _ => return Err(invalid_request(format!("Usage: /account {verb} <on|off>"))),
                        };
                        let key = if verb == "warmup" { "window_warmup" } else { "resume_after_reset" };
                        (key, serde_json::json!(enabled))
                    }
                    "wait" => {
                        let minutes = value.parse::<u64>().ok().filter(|n| *n <= 1440)
                            .ok_or_else(|| invalid_request("Usage: /account wait <minutes: 0..1440>"))?;
                        ("max_reset_wait_minutes", serde_json::json!(minutes))
                    }
                    "reset-credits" => {
                        let mode = match value {
                            "never" => "never",
                            "when-pool-exhausted" | "when_pool_exhausted" => "when_pool_exhausted",
                            _ => return Err(invalid_request("Usage: /account reset-credits <never|when-pool-exhausted>")),
                        };
                        ("auto_reset_credits", serde_json::json!(mode))
                    }
                    _ => unreachable!("matched account-pool setting"),
                };
                self.write_mobile_pool_setting(config_processor, queues, key, setting.clone()).await?;
                let config = self.load_latest_config().await;
                let effective = serde_json::to_value(&config.account_pool)
                    .map_err(|err| internal_error(err.to_string()))?;
                let notice = if effective.get(key) == Some(&setting) {
                    "Saved. Applies after configuration refresh."
                } else {
                    "Saved, but overridden by higher-priority configuration."
                };
                Ok(format!("{notice}\n\n{}", view::settings(&config.account_pool)))
            }
            (_, "strategy") => {
                let strategy = match value {
                    "" => self.load_latest_config().await.account_pool.effective_rotation_strategy(),
                    "fill-first" | "fill_first" => codex_config::AccountPoolRotationStrategy::FillFirst,
                    "earliest-reset" | "earliest_reset" => codex_config::AccountPoolRotationStrategy::EarliestReset,
                    _ => return Err(invalid_request("Usage: /account strategy [fill-first|earliest-reset]")),
                };
                if !value.is_empty() {
                    let value = serde_json::to_value(strategy).map_err(|err| internal_error(err.to_string()))?;
                    self.write_mobile_pool_setting(config_processor, queues, "rotation_strategy", value).await?;
                    self.get_account_pool_response().await?;
                    let effective = self.load_latest_config().await.account_pool.effective_rotation_strategy();
                    if effective != strategy {
                        return Ok("Saved, but overridden by a higher-priority configuration. Use /account strategy to see the effective strategy.".into());
                    }
                }
                let name = match strategy {
                    codex_config::AccountPoolRotationStrategy::FillFirst => "fill-first",
                    codex_config::AccountPoolRotationStrategy::EarliestReset => "earliest-reset",
                };
                Ok(format!("Strategy: {name}\nUsed at the next automatic selection. Select now: /account auto"))
            }
            (_, "help") if value.is_empty() => Ok("/account list [page] — accounts and quota\n/account show <label|@selector> — details\n/account use <label|@selector> — select an available account\n/account retry <label|@selector> — clear local cooldown and probe again\n/account auto — let the strategy select now\n/account strategy [fill-first|earliest-reset]\n/account settings — effective settings and controls\n/account warmup [page|now|on|off] — status or request one pass\nQuote names containing spaces. Use @selectors for duplicate names. Selection does not permanently pin an account.".into()),
            _ => Err(invalid_request("Unknown /account command. Use /account help.")),
        }
    }
    async fn write_mobile_pool_setting(
        &self,
        config_processor: &ConfigRequestProcessor,
        queues: &RequestSerializationQueues,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), JSONRPCErrorError> {
        let processor = config_processor.clone();
        let key_path = format!("account_pool.{key}");
        let (tx, rx) = tokio::sync::oneshot::channel();
        queues
            .enqueue_background(
                RequestSerializationQueueKey::Global("config"),
                RequestSerializationAccess::Exclusive,
                async move {
                    let result = processor
                        .batch_write(ConfigBatchWriteParams {
                            edits: vec![ConfigEdit {
                                key_path,
                                value,
                                merge_strategy: MergeStrategy::Replace,
                            }],
                            file_path: None,
                            expected_version: None,
                            reload_user_config: true,
                        })
                        .await;
                    let _ = tx.send(result);
                },
            )
            .await;
        rx.await
            .map_err(|_| internal_error("configuration update interrupted"))??;
        self.get_account_pool_response().await?;
        Ok(())
    }
}
