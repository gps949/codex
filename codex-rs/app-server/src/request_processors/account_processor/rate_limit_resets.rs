use super::*;
use crate::reset_credit_journal;
use crate::reset_credit_journal::ManualResetCreditJournal;

const RATE_LIMIT_RESET_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);
const RATE_LIMIT_RESET_DETAILS_REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 5);
#[cfg(debug_assertions)]
const RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR: &str =
    "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";

struct ResetProfileIdentity {
    profile_id: codex_login::AccountProfileId,
    manager: Arc<AuthManager>,
    owner_generation: u64,
    account_id: Option<String>,
    user_id: Option<String>,
    selection: (Option<String>, Option<u64>),
    shared_selection: (Option<codex_login::AccountProfileId>, u64),
}

impl ResetProfileIdentity {
    async fn still_owned(&self) -> bool {
        self.manager.reload().await;
        self.manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation
            == self.owner_generation
            && self.manager.auth_cached().is_some_and(|auth| {
                auth.get_account_id() == self.account_id
                    && auth.get_chatgpt_user_id() == self.user_id
            })
    }
}

fn eligible_reset_credit(credit: &BackendRateLimitResetCreditDetails) -> bool {
    !credit.id.is_empty()
        && credit.id.len() <= 256
        && credit.reset_type == "codex_rate_limits"
        && credit.status == "available"
        && credit.expires_at.as_deref().is_none_or(|expires| {
            chrono::DateTime::parse_from_rfc3339(expires)
                .is_ok_and(|expires| expires > chrono::Utc::now())
        })
}

impl AccountRequestProcessor {
    pub(super) async fn detailed_rate_limit_reset_credits(
        client: &BackendClient,
    ) -> Option<RateLimitResetCreditsSummary> {
        let details = match tokio::time::timeout(
            RATE_LIMIT_RESET_DETAILS_REQUEST_TIMEOUT,
            client.list_rate_limit_reset_credits(),
        )
        .await
        {
            Ok(Ok(details)) => details,
            Ok(Err(err)) => {
                tracing::warn!(
                    "failed to fetch rate limit reset credit details; falling back to the usage response: {err}"
                );
                return None;
            }
            Err(_) => {
                tracing::warn!(
                    "rate limit reset credit detail request timed out; falling back to the usage response"
                );
                return None;
            }
        };

        match rate_limit_reset_credits_from_backend(details) {
            Ok(summary) => Some(summary),
            Err(err) => {
                tracing::warn!(
                    "failed to parse rate limit reset credit details; falling back to the usage response: {err}"
                );
                None
            }
        }
    }

    pub(crate) async fn consume_account_rate_limit_reset_credit(
        &self,
        params: ConsumeAccountRateLimitResetCreditParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if params.idempotency_key.is_empty() {
            return Err(invalid_request("idempotencyKey must not be empty"));
        }
        if params.idempotency_key.len() > reset_credit_journal::MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(invalid_request("idempotencyKey must not exceed 128 bytes"));
        }
        if params.credit_id.as_deref().is_some_and(str::is_empty) {
            return Err(invalid_request("creditId must not be empty"));
        }
        if params
            .credit_id
            .as_ref()
            .is_some_and(|id| id.len() > reset_credit_journal::MAX_CREDIT_ID_BYTES)
        {
            return Err(invalid_request("creditId must not exceed 256 bytes"));
        }
        if params
            .expected_owner_key
            .as_deref()
            .is_some_and(|owner| !reset_credit_journal::valid_owner_key(owner))
        {
            return Err(invalid_request(
                "expectedOwnerKey must be a valid reset owner key",
            ));
        }

        let (client, auth, profile, owner_generation) =
            self.rate_limit_reset_backend_client().await?;
        let owner_digest = reset_credit_journal::owner_key(&self.config.chatgpt_base_url, &auth)
            .map_err(invalid_request)?;
        if params
            .expected_owner_key
            .as_deref()
            .is_some_and(|owner| owner != owner_digest)
        {
            return Err(invalid_request(
                "account changed since reset confirmation; return to the original account to retry",
            ));
        }
        let request_timeout = RATE_LIMIT_RESET_REQUEST_TIMEOUT;
        #[cfg(debug_assertions)]
        let request_timeout = std::env::var(RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(request_timeout);
        let deadline = tokio::time::Instant::now() + request_timeout;
        let store =
            codex_login::AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf());
        let _spending_lock = tokio::time::timeout_at(deadline, async {
            loop {
                if let Some(lock) = store.try_lock_reset_credit()? {
                    break Ok::<_, std::io::Error>(lock);
                }
                tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
            }
        })
        .await
        .map_err(|_| {
            internal_error("another reset operation is running; retry with the same idempotencyKey")
        })?
        .map_err(|error| {
            internal_error(format!("failed to lock reset credit spending: {error}"))
        })?;
        let mut journal =
            ManualResetCreditJournal::load(&self.config.codex_home).map_err(internal_error)?;
        let known_credit = journal
            .known_credit(
                &owner_digest,
                &params.idempotency_key,
                params.credit_id.as_deref(),
            )
            .map_err(invalid_request)?
            .map(str::to_owned);
        let terminal = journal
            .terminal_outcome(&owner_digest, &params.idempotency_key)
            .map_err(invalid_request)?;
        if terminal.is_none() {
            journal
                .check_pending(&owner_digest, &params.idempotency_key)
                .map_err(invalid_request)?;
        }
        let inventory = if known_credit.is_some() {
            None
        } else {
            Some(
                tokio::time::timeout_at(deadline, client.list_rate_limit_reset_credits())
                    .await
                    .map_err(|_| internal_error("rate limit reset credit check timed out"))?
                    .map_err(|error| {
                        internal_error(format!("failed to validate reset credit: {error}"))
                    })?,
            )
        };
        let credit = inventory.as_ref().and_then(|inventory| {
            inventory
                .credits
                .iter()
                .filter(|credit| {
                    params.credit_id.as_deref().is_none_or(|id| credit.id == id)
                        && eligible_reset_credit(credit)
                })
                .min_by_key(|credit| {
                    (
                        credit
                            .expires_at
                            .as_deref()
                            .and_then(|expires| chrono::DateTime::parse_from_rfc3339(expires).ok())
                            .map_or(chrono::DateTime::<chrono::Utc>::MAX_UTC, |expires| {
                                expires.with_timezone(&chrono::Utc)
                            }),
                        credit.id.as_str(),
                    )
                })
        });
        if known_credit.is_none() && credit.is_none() {
            if params.credit_id.is_some() {
                return Err(invalid_request(
                    "reset credit is unavailable, expired, or has a different quota scope",
                ));
            }
            return Ok(Some(
                ConsumeAccountRateLimitResetCreditResponse {
                    outcome: ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
                }
                .into(),
            ));
        }
        let credit_id = known_credit
            .as_deref()
            .or_else(|| credit.map(|credit| credit.id.as_str()))
            .ok_or_else(|| internal_error("No reset credit selected"))?;
        let pool = self.execution_account_pool.account_pool();
        if profile.is_some() && pool.is_none() {
            return Err(invalid_request(
                "account pool changed before reset; refresh before retrying",
            ));
        }
        let probe = if let (Some(profile), Some(pool)) = (profile.as_ref(), pool.as_ref()) {
            store.try_synchronize(pool).map_err(|error| {
                internal_error(format!(
                    "failed to synchronize reset credit account: {error}"
                ))
            })?;
            let selected = self.get_account_pool_response().await?;
            let shared_selection = store
                .try_load()
                .map_err(|error| internal_error(error.to_string()))?
                .map(|state| (state.active_profile_id, state.selection_revision));
            if (selected.active_profile_id, selected.active_generation) != profile.selection
                || shared_selection.as_ref() != Some(&profile.shared_selection)
                || !profile.still_owned().await
                || !store
                    .validate_profile_auth(pool, &profile.profile_id, &auth)
                    .map_err(|error| internal_error(error.to_string()))?
            {
                return Err(invalid_request(
                    "account changed before reset; refresh before retrying",
                ));
            }
            store
                .capture_quota_probe(pool, &profile.profile_id, &auth)
                .map_err(|error| internal_error(error.to_string()))?
        } else {
            self.auth_manager.reload().await;
            if self
                .auth_manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation
                != owner_generation
                || !self.auth_manager.auth_cached().is_some_and(|current| {
                    current.get_account_id() == auth.get_account_id()
                        && current.get_chatgpt_user_id() == auth.get_chatgpt_user_id()
                })
            {
                return Err(invalid_request(
                    "account changed before reset; refresh before retrying",
                ));
            }
            None
        };
        if let Some(outcome) = terminal {
            return Ok(Some(
                ConsumeAccountRateLimitResetCreditResponse { outcome }.into(),
            ));
        }
        if credit.is_some_and(|credit| !eligible_reset_credit(credit)) {
            return Err(invalid_request(
                "reset credit expired before spending; refresh available credits",
            ));
        }
        journal
            .remember(&owner_digest, &params.idempotency_key, credit_id)
            .map_err(internal_error)?;
        let response = tokio::time::timeout_at(
            deadline,
            client.consume_rate_limit_reset_credit_by_id(&params.idempotency_key, credit_id),
        )
        .await
        .map_err(|_| internal_error("rate limit reset consume timed out"))?
        .map_err(|err| internal_error(format!("failed to consume rate limit reset: {err}")))?;
        let outcome = match response.code {
            BackendConsumeRateLimitResetCreditCode::Reset => {
                ConsumeAccountRateLimitResetCreditOutcome::Reset
            }
            BackendConsumeRateLimitResetCreditCode::NothingToReset => {
                ConsumeAccountRateLimitResetCreditOutcome::NothingToReset
            }
            BackendConsumeRateLimitResetCreditCode::NoCredit => {
                ConsumeAccountRateLimitResetCreditOutcome::NoCredit
            }
            BackendConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
            }
        };
        journal
            .complete(&owner_digest, &params.idempotency_key, outcome)
            .map_err(|_| {
                internal_error("reset response could not be saved; retry the original operation")
            })?;
        if let (Some(profile), Some(pool), Some(probe)) = (profile.as_ref(), pool.as_ref(), probe)
            && profile.still_owned().await
        {
            let confirmed = match response.code {
                BackendConsumeRateLimitResetCreditCode::Reset if response.windows_reset >= 2 => {
                    store.confirm_quota_reset(pool, probe, chrono::Utc::now())
                }
                BackendConsumeRateLimitResetCreditCode::Reset
                | BackendConsumeRateLimitResetCreditCode::NothingToReset
                | BackendConsumeRateLimitResetCreditCode::AlreadyRedeemed => {
                    if let Ok(Ok(observed)) = tokio::time::timeout_at(
                        deadline,
                        client.get_rate_limits_with_reset_credits(),
                    )
                    .await
                        && profile.still_owned().await
                    {
                        store.reconcile_quota_probe(
                            pool,
                            probe,
                            codex_login::account_runtime_state::AccountQuotaEvidence {
                                rate_limits: &observed.rate_limits,
                                ordinary_usage_allowed: observed.ordinary_usage_allowed,
                                account_id: observed.account_id.as_deref(),
                                user_id: observed.user_id.as_deref(),
                            },
                        )
                    } else {
                        Ok(false)
                    }
                }
                BackendConsumeRateLimitResetCreditCode::NoCredit => Ok(false),
            };
            if let Err(error) = confirmed {
                tracing::warn!(%error, "failed to persist confirmed reset-credit quota recovery");
            }
        }
        Ok(Some(
            ConsumeAccountRateLimitResetCreditResponse { outcome }.into(),
        ))
    }

    async fn rate_limit_reset_backend_client(
        &self,
    ) -> Result<
        (
            BackendClient,
            codex_login::CodexAuth,
            Option<ResetProfileIdentity>,
            u64,
        ),
        JSONRPCErrorError,
    > {
        let pool = self.get_account_pool_response().await?;
        let owner_generation = self
            .auth_manager
            .auth_change_state_receiver()
            .borrow()
            .owner_generation;
        let Some((auth, http_client_factory)) =
            self.auth_manager.auth_with_http_client_factory().await
        else {
            return Err(invalid_request(
                "codex account authentication required for rate limit reset credits",
            ));
        };
        if !auth.uses_codex_backend() {
            return Err(invalid_request(
                "chatgpt authentication required for rate limit reset credits",
            ));
        }

        let profile = if pool.enabled {
            let shared_selection =
                codex_login::AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf())
                    .try_load()
                    .map_err(|error| internal_error(error.to_string()))?
                    .ok_or_else(|| {
                        internal_error(
                            "account state is busy; retry the reset with the same idempotencyKey",
                        )
                    })?;
            let token = auth.get_token().ok();
            let account_id = auth.get_account_id();
            let user_id = auth.get_chatgpt_user_id();
            let mut managers = self.execution_account_pool.auth_managers();
            managers.sort_by_key(|(id, _)| Some(id.as_str()) != pool.active_profile_id.as_deref());
            let (profile_id, manager) = managers
                .into_iter()
                .find(|(_, manager)| {
                    manager.auth_cached().is_some_and(|candidate| {
                        token.is_some()
                            && candidate.get_token().ok() == token
                            && candidate.get_account_id() == account_id
                            && candidate.get_chatgpt_user_id() == user_id
                    })
                })
                .ok_or_else(|| {
                    internal_error(
                        "account changed while preparing rate limit reset; retry the request",
                    )
                })?;
            let owner_generation = manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation;
            if shared_selection
                .active_profile_id
                .as_ref()
                .is_some_and(|id| id != &profile_id)
            {
                return Err(invalid_request(
                    "account selection changed before reset; refresh before retrying",
                ));
            }
            Some(ResetProfileIdentity {
                profile_id,
                manager,
                owner_generation,
                account_id,
                user_id,
                selection: (pool.active_profile_id, pool.active_generation),
                shared_selection: (
                    shared_selection.active_profile_id,
                    shared_selection.selection_revision,
                ),
            })
        } else {
            None
        };
        Ok((
            BackendClient::from_auth(
                self.config.chatgpt_base_url.clone(),
                &auth,
                http_client_factory,
            ),
            auth,
            profile,
            owner_generation,
        ))
    }
}

fn rate_limit_reset_credits_from_backend(
    details: BackendRateLimitResetCreditsDetails,
) -> Result<RateLimitResetCreditsSummary, String> {
    let credits = details
        .credits
        .into_iter()
        .map(rate_limit_reset_credit_from_backend)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RateLimitResetCreditsSummary {
        available_count: details.available_count,
        credits: Some(credits),
    })
}

fn rate_limit_reset_credit_from_backend(
    credit: BackendRateLimitResetCreditDetails,
) -> Result<RateLimitResetCredit, String> {
    let reset_type = match credit.reset_type.as_str() {
        "codex_rate_limits" => RateLimitResetType::CodexRateLimits,
        _ => RateLimitResetType::Unknown,
    };
    let status = match credit.status.as_str() {
        "available" => RateLimitResetCreditStatus::Available,
        "redeeming" => RateLimitResetCreditStatus::Redeeming,
        "redeemed" => RateLimitResetCreditStatus::Redeemed,
        _ => RateLimitResetCreditStatus::Unknown,
    };
    let granted_at = rate_limit_reset_credit_timestamp(&credit.granted_at)
        .map_err(|err| format!("invalid granted_at for credit `{}`: {err}", credit.id))?;
    let expires_at = credit
        .expires_at
        .as_deref()
        .map(rate_limit_reset_credit_timestamp)
        .transpose()
        .map_err(|err| format!("invalid expires_at for credit `{}`: {err}", credit.id))?;

    Ok(RateLimitResetCredit {
        id: credit.id,
        reset_type,
        status,
        granted_at,
        expires_at,
        title: credit.title,
        description: credit.description,
    })
}

fn rate_limit_reset_credit_timestamp(timestamp: &str) -> Result<i64, String> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|timestamp| timestamp.timestamp())
        .map_err(|err| format!("failed to parse timestamp `{timestamp}`: {err}"))
}
