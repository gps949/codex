use super::*;

#[cfg(test)]
#[path = "manual_reset_credit_journal.rs"]
mod manual_reset_credit_journal;

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
        if params.credit_id.as_deref().is_some_and(str::is_empty) {
            return Err(invalid_request("creditId must not be empty"));
        }

        let (client, profile) = self.rate_limit_reset_backend_client().await?;
        let request_timeout = RATE_LIMIT_RESET_REQUEST_TIMEOUT;
        #[cfg(debug_assertions)]
        let request_timeout = std::env::var(RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(request_timeout);
        let response = tokio::time::timeout(request_timeout, async {
            match params.credit_id.as_deref() {
                Some(credit_id) => {
                    client
                        .consume_rate_limit_reset_credit_by_id(&params.idempotency_key, credit_id)
                        .await
                }
                None => {
                    client
                        .consume_rate_limit_reset_credit(&params.idempotency_key)
                        .await
                }
            }
        })
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
        if matches!(
            outcome,
            ConsumeAccountRateLimitResetCreditOutcome::Reset
                | ConsumeAccountRateLimitResetCreditOutcome::NothingToReset
        ) && let Some(profile) = profile
        {
            // Redemption belongs to the request's seat, even if another account was selected
            // while the backend call was in flight. A re-login may also replace this owner.
            profile.manager.reload().await;
            let auth = profile.manager.auth().await;
            let owner_generation = profile
                .manager
                .auth_change_state_receiver()
                .borrow()
                .owner_generation;
            if owner_generation == profile.owner_generation
                && auth.as_ref().is_some_and(|auth| {
                    auth.get_account_id() == profile.account_id
                        && auth.get_chatgpt_user_id() == profile.user_id
                })
            {
                let reset_at = chrono::Utc::now();
                if let Err(error) =
                    codex_login::AccountRuntimeStateStore::new(self.config.codex_home.to_path_buf())
                        .record_quota_reset(&profile.profile_id, reset_at)
                {
                    tracing::warn!(%error, "failed to persist redeemed account quota reset");
                } else if let Some(pool) = self.execution_account_pool.account_pool()
                    && pool.auth_managers().iter().any(|(id, manager)| {
                        id == &profile.profile_id && Arc::ptr_eq(manager, &profile.manager)
                    })
                    && let Err(error) = pool.apply_quota_reset(&profile.profile_id, reset_at)
                {
                    tracing::warn!(%error, "failed to apply redeemed account quota reset");
                }
            }
        }
        Ok(Some(
            ConsumeAccountRateLimitResetCreditResponse { outcome }.into(),
        ))
    }

    async fn rate_limit_reset_backend_client(
        &self,
    ) -> Result<(BackendClient, Option<ResetProfileIdentity>), JSONRPCErrorError> {
        let pool = self.get_account_pool_response().await?;
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
            Some(ResetProfileIdentity {
                profile_id,
                manager,
                owner_generation,
                account_id,
                user_id,
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
            profile,
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
