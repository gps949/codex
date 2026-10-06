//! A bounded semantic advisor that cannot replace inference or authorize operations.

use crate::DecisionAdvice;
use crate::DecisionAdvisorFallback;
use crate::DecisionAdvisorMode;
use crate::DecisionAdvisorSettings;
use crate::DecisionCandidate;
use crate::DecisionSearchRequest;
use crate::DecisionSearchScope;
use crate::RoutingTaskComplexity;
use crate::decision_advisor_protocol::MAX_BODY_BYTES;
use crate::decision_advisor_protocol::parse_ranking;
use crate::decision_advisor_protocol::request_body;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use url::Url;

const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CLIENTS: usize = 8;
const CACHE_TTL: Duration = Duration::from_secs(60);
const FAILURE_TTL: Duration = Duration::from_secs(5);
type CacheKey = [u8; 32];

#[derive(Default)]
struct State {
    cache: VecDeque<(CacheKey, Instant, DecisionAdvice)>,
    flights: HashMap<CacheKey, Weak<tokio::sync::Mutex<()>>>,
    clients: VecDeque<(HttpClientFactory, String, HttpClient)>,
}

#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    cache_hits: AtomicU64,
    timeouts: AtomicU64,
    rankings: AtomicU64,
    applied: AtomicU64,
    fallbacks: AtomicU64,
    comparisons: AtomicU64,
    would_change: AtomicU64,
    empty_search_recovered: AtomicU64,
    latency_total_ms: AtomicU64,
    latency_max_ms: AtomicU64,
}

/// Anonymous, process-local diagnostics; no inputs, identities, endpoints, or keys.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct DecisionAdvisorStats {
    pub requests: u64,
    pub cache_hits: u64,
    pub timeouts: u64,
    pub rankings: u64,
    pub applied: u64,
    pub fallbacks: u64,
    pub comparisons: u64,
    pub would_change: u64,
    pub empty_search_recovered: u64,
    pub latency_total_ms: u64,
    pub latency_max_ms: u64,
}

pub struct DecisionAdvisor {
    state: Mutex<State>,
    concurrency: Semaphore,
    counters: Counters,
}

impl Default for DecisionAdvisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            concurrency: Semaphore::new(2),
            counters: Counters::default(),
        }
    }
}

/// Shared by tool discovery across sessions without retaining secrets or an unbounded catalog.
pub fn decision_advisor() -> &'static DecisionAdvisor {
    static SERVICE: OnceLock<DecisionAdvisor> = OnceLock::new();
    SERVICE.get_or_init(DecisionAdvisor::default)
}

impl DecisionAdvisor {
    /// Explicit task assessment; callers gate routing and consent independently of tool search.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn assess_task(
        &self,
        settings: &DecisionAdvisorSettings,
        factory: &HttpClientFactory,
        task: &str,
        credential: Option<&str>,
    ) -> Result<RoutingTaskComplexity, DecisionAdvisorFallback> {
        settings
            .validate_service()
            .map_err(|_| DecisionAdvisorFallback::InvalidConfiguration)?;
        let candidates = [
            ("simple", "A short, self-contained translation, wording, or formatting task."),
            ("standard", "Ordinary coding, writing, explanation, or analysis with a clear scope."),
            ("demanding", "Complex architecture, investigation, concurrency, migration, or broad reasoning."),
            ("high_stakes", "Security, authorization, production, financial, legal, medical, or destructive work."),
            ("unknown", "The available task description is ambiguous or insufficient."),
        ]
        .into_iter()
        .map(|(id, description)| DecisionCandidate {
            id: id.into(),
            description: description.into(),
        })
        .collect::<Vec<_>>();
        let request = DecisionSearchRequest {
            scope: DecisionSearchScope::Models,
            query: task,
            candidates: &candidates,
            catalog_revision: b"task-complexity-v1",
        };
        let advice = tokio::time::timeout(
            settings.timeout,
            self.rank_within_budget(settings, factory, &request, credential),
        )
        .await
        .map_err(|_| DecisionAdvisorFallback::TimedOut)?;
        match advice {
            DecisionAdvice::Fallback(reason) => Err(reason),
            DecisionAdvice::Ranked(ids) => match ids.first().map(String::as_str) {
                Some("simple") => Ok(RoutingTaskComplexity::Simple),
                Some("standard") => Ok(RoutingTaskComplexity::Standard),
                Some("demanding") => Ok(RoutingTaskComplexity::Demanding),
                Some("high_stakes") => Ok(RoutingTaskComplexity::HighStakes),
                Some("unknown") => Ok(RoutingTaskComplexity::Unknown),
                Some(_) | None => Err(DecisionAdvisorFallback::InvalidResponse),
            },
        }
    }

    /// Dropping this future cancels transport and releases both flight and concurrency guards.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn rank(
        &self,
        settings: &DecisionAdvisorSettings,
        factory: &HttpClientFactory,
        request: DecisionSearchRequest<'_>,
        credential: Option<&str>,
    ) -> DecisionAdvice {
        if settings.mode == DecisionAdvisorMode::Off {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::Disabled);
        }
        let started = Instant::now();
        let outcome = match tokio::time::timeout(
            settings.timeout,
            self.rank_within_budget(settings, factory, &request, credential),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => DecisionAdvice::Fallback(DecisionAdvisorFallback::TimedOut),
        };
        if outcome == DecisionAdvice::Fallback(DecisionAdvisorFallback::TimedOut) {
            self.counters.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        match &outcome {
            DecisionAdvice::Ranked(_) => {
                self.counters.rankings.fetch_add(1, Ordering::Relaxed);
            }
            DecisionAdvice::Fallback(_) => {
                self.counters.fallbacks.fetch_add(1, Ordering::Relaxed);
            }
        }
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.counters
            .latency_total_ms
            .fetch_add(elapsed_ms, Ordering::Relaxed);
        self.counters
            .latency_max_ms
            .fetch_max(elapsed_ms, Ordering::Relaxed);
        let stats = self.stats();
        tracing::debug!(
            requests = stats.requests,
            cache_hits = stats.cache_hits,
            timeouts = stats.timeouts,
            rankings = stats.rankings,
            applied = stats.applied,
            fallbacks = stats.fallbacks,
            elapsed_ms,
            comparisons = stats.comparisons,
            would_change = stats.would_change,
            empty_search_recovered = stats.empty_search_recovered,
            mode = ?settings.mode,
            scope = ?request.scope,
            outcome = match &outcome { DecisionAdvice::Ranked(_) => "ranked", DecisionAdvice::Fallback(_) => "fallback" },
            "Decision advisor completed"
        );
        outcome
    }

    async fn rank_within_budget(
        &self,
        settings: &DecisionAdvisorSettings,
        factory: &HttpClientFactory,
        request: &DecisionSearchRequest<'_>,
        credential: Option<&str>,
    ) -> DecisionAdvice {
        let started = Instant::now();
        if settings.validate_service().is_err() {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::InvalidConfiguration);
        }
        let Ok(endpoint) = Url::parse(&settings.endpoint) else {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::InvalidConfiguration);
        };
        let credential = credential.filter(|value| !value.is_empty());
        if credential.is_none() && !settings.allows_unauthenticated_local_service(&endpoint) {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::MissingCredential);
        }
        let Ok(permit) = factory.network_policy().acquire(&endpoint) else {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::NetworkDenied);
        };
        let body = match request_body(settings, request) {
            Ok(body) => body,
            Err(reason) => return DecisionAdvice::Fallback(reason),
        };
        let Ok(encoded) = serde_json::to_vec(&body) else {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::OversizedInput);
        };
        if encoded.len() > MAX_BODY_BYTES {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::OversizedInput);
        }
        let Ok(settings_bytes) = serde_json::to_vec(settings) else {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::InvalidConfiguration);
        };
        let mut digest = Sha256::new();
        digest.update(b"codex-decision-advisor-v1\0");
        digest.update(settings_bytes);
        digest.update(Sha256::digest(credential.unwrap_or_default().as_bytes()));
        digest.update(request.catalog_revision.len().to_be_bytes());
        digest.update(request.catalog_revision);
        digest.update(encoded);
        let key: CacheKey = digest.finalize().into();
        let flight = {
            let mut state = self.lock_state();
            if let Some(cached) = Self::cached(&mut state, key) {
                self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
                return cached;
            }
            state.flights.retain(|_, flight| flight.strong_count() != 0);
            if let Some(flight) = state.flights.get(&key).and_then(Weak::upgrade) {
                flight
            } else {
                if state.flights.len() >= MAX_CACHE_ENTRIES {
                    return DecisionAdvice::Fallback(DecisionAdvisorFallback::Busy);
                }
                let flight = Arc::new(tokio::sync::Mutex::new(()));
                state.flights.insert(key, Arc::downgrade(&flight));
                flight
            }
        };
        let _flight = flight.lock_owned().await;
        if permit.check().is_err() {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::NetworkDenied);
        }
        if let Some(cached) = Self::cached(&mut self.lock_state(), key) {
            self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
            return cached;
        }
        let Ok(_concurrency) = self.concurrency.try_acquire() else {
            return DecisionAdvice::Fallback(DecisionAdvisorFallback::Busy);
        };
        let client = self.client(factory, &endpoint);
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let operation = async {
            let mut builder = client.post(endpoint).timeout(settings.timeout).json(&body);
            if let Some(credential) = credential {
                builder = builder.bearer_auth(credential);
            }
            let mut response = builder
                .send()
                .await
                .map_err(|_| DecisionAdvisorFallback::Unavailable)?;
            if !response.status().is_success()
                || response
                    .content_length()
                    .is_some_and(|length| length > MAX_BODY_BYTES as u64)
            {
                return Err(DecisionAdvisorFallback::Unavailable);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| DecisionAdvisorFallback::Unavailable)?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                    return Err(DecisionAdvisorFallback::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = serde_json::from_slice(&bytes)
                .map_err(|_| DecisionAdvisorFallback::InvalidResponse)?;
            parse_ranking(settings, request.scope, request.candidates, body)
        };
        let remaining = settings
            .timeout
            .saturating_sub(started.elapsed() + Duration::from_millis(5));
        let result = match tokio::time::timeout(remaining, permit.run(operation)).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DecisionAdvisorFallback::NetworkDenied),
            Err(_) => Err(DecisionAdvisorFallback::TimedOut),
        };
        let outcome = match result {
            Ok(ranking) => DecisionAdvice::Ranked(ranking),
            Err(reason) => DecisionAdvice::Fallback(reason),
        };
        let expires = Instant::now()
            + match outcome {
                DecisionAdvice::Ranked(_) => CACHE_TTL,
                DecisionAdvice::Fallback(_) => FAILURE_TTL,
            };
        let mut state = self.lock_state();
        if state.cache.len() == MAX_CACHE_ENTRIES {
            state.cache.pop_front();
        }
        state.cache.push_back((key, expires, outcome.clone()));
        outcome
    }

    fn cached(state: &mut State, key: CacheKey) -> Option<DecisionAdvice> {
        let now = Instant::now();
        state.cache.retain(|(_, expires, _)| *expires > now);
        let index = state
            .cache
            .iter()
            .position(|(cached, _, _)| *cached == key)?;
        let entry = state.cache.remove(index)?;
        let result = entry.2.clone();
        state.cache.push_back(entry);
        Some(result)
    }

    fn client(&self, factory: &HttpClientFactory, endpoint: &Url) -> HttpClient {
        let mut state = self.lock_state();
        if let Some((_, _, client)) = state
            .clients
            .iter()
            .find(|(cached, url, _)| cached == factory && url == endpoint.as_str())
        {
            return client.clone();
        }
        let client = RouteAwareClientPool::new_without_redirects_or_request_logging(
            factory.clone(),
            ClientRouteClass::Other,
        )
        .into_client();
        if state.clients.len() == MAX_CLIENTS {
            state.clients.pop_front();
        }
        state
            .clients
            .push_back((factory.clone(), endpoint.to_string(), client.clone()));
        client
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a dry-run comparison without retaining queries, tool names or account data.
    pub fn record_comparison(&self, baseline: &[usize], ranked: &[usize]) {
        self.counters.comparisons.fetch_add(1, Ordering::Relaxed);
        if baseline != ranked {
            self.counters.would_change.fetch_add(1, Ordering::Relaxed);
        }
        if baseline.is_empty() && !ranked.is_empty() {
            self.counters
                .empty_search_recovered
                .fetch_add(1, Ordering::Relaxed);
        }
        tracing::debug!(
            would_change = baseline != ranked,
            empty_search_recovered = baseline.is_empty() && !ranked.is_empty(),
            "Decision advisor search comparison"
        );
    }

    pub fn record_application(&self) {
        self.counters.applied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> DecisionAdvisorStats {
        DecisionAdvisorStats {
            requests: self.counters.requests.load(Ordering::Relaxed),
            cache_hits: self.counters.cache_hits.load(Ordering::Relaxed),
            timeouts: self.counters.timeouts.load(Ordering::Relaxed),
            rankings: self.counters.rankings.load(Ordering::Relaxed),
            applied: self.counters.applied.load(Ordering::Relaxed),
            fallbacks: self.counters.fallbacks.load(Ordering::Relaxed),
            comparisons: self.counters.comparisons.load(Ordering::Relaxed),
            would_change: self.counters.would_change.load(Ordering::Relaxed),
            empty_search_recovered: self.counters.empty_search_recovered.load(Ordering::Relaxed),
            latency_total_ms: self.counters.latency_total_ms.load(Ordering::Relaxed),
            latency_max_ms: self.counters.latency_max_ms.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "decision_advisor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "decision_advisor_model_routing_tests.rs"]
mod model_routing_tests;
