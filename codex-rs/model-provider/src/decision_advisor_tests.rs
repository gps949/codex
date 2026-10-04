use super::*;
use crate::DecisionAdvisorProvider;
use crate::DecisionCandidate;
use crate::DecisionSearchScope;
use crate::decision_advisor_protocol::parse_ranking;
use codex_http_client::NetworkPolicyController;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn candidates() -> Vec<DecisionCandidate> {
    vec![
        DecisionCandidate {
            id: "t0".into(),
            description: "Search code repositories".into(),
        },
        DecisionCandidate {
            id: "t1".into(),
            description: "Check the weather forecast".into(),
        },
    ]
}

fn answer(model: &str) -> Value {
    json!({"model":model, "answers":{"tool":{
        "type":"choice", "choice":"t1", "confidence":0.9,
        "probabilities":{"t0":0.09,"t1":0.9,"none":0.01}
    }}})
}

fn settings(server: &MockServer, provider: DecisionAdvisorProvider) -> DecisionAdvisorSettings {
    DecisionAdvisorSettings {
        mode: DecisionAdvisorMode::Rank,
        provider,
        endpoint: format!("{}/advisor", server.uri()),
        model: match provider {
            DecisionAdvisorProvider::Typesafe => "jev-test",
            DecisionAdvisorProvider::Cloudflare => "clef-flash",
        }
        .into(),
        allow_local_http: true,
        ..Default::default()
    }
}

fn factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

#[tokio::test]
async fn decision_advisor_adapters_use_systemone_shapes_and_deduplicate_same_input() {
    for provider in [
        DecisionAdvisorProvider::Typesafe,
        DecisionAdvisorProvider::Cloudflare,
    ] {
        let server = MockServer::start().await;
        let settings = settings(&server, provider);
        let candidates = candidates();
        let request = DecisionSearchRequest {
            scope: DecisionSearchScope::Tools,
            query: "看看天气",
            candidates: &candidates,
            catalog_revision: b"catalog-1",
        };
        let body = request_body(&settings, &request).unwrap();
        let response = match provider {
            DecisionAdvisorProvider::Typesafe => answer("jev-test"),
            DecisionAdvisorProvider::Cloudflare => {
                json!({"result":answer("clef-flash"),"success":true,"errors":[],"messages":[]})
            }
        };
        Mock::given(method("POST"))
            .and(path("/advisor"))
            .and(header("Authorization", "Bearer separate-test-key"))
            .and(body_json(body))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(response)
                    .set_delay(Duration::from_millis(30)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let advisor = DecisionAdvisor::default();
        let factory = factory();
        let first = advisor.rank(&settings, &factory, request, Some("separate-test-key"));
        let second = advisor.rank(
            &settings,
            &factory,
            DecisionSearchRequest {
                scope: DecisionSearchScope::Tools,
                query: "看看天气",
                candidates: &candidates,
                catalog_revision: b"catalog-1",
            },
            Some("separate-test-key"),
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(
            (first, second),
            (
                DecisionAdvice::Ranked(vec!["t1".into(), "t0".into()]),
                DecisionAdvice::Ranked(vec!["t1".into(), "t0".into()])
            )
        );
        let stats = advisor.stats();
        assert!(stats.latency_total_ms >= stats.latency_max_ms);
        assert!(stats.latency_max_ms >= 25);
        assert_eq!(
            stats,
            DecisionAdvisorStats {
                requests: 1,
                cache_hits: 1,
                rankings: 2,
                latency_total_ms: stats.latency_total_ms,
                latency_max_ms: stats.latency_max_ms,
                ..Default::default()
            }
        );
    }
}

#[tokio::test]
async fn decision_advisor_off_and_network_denial_never_send_requests() {
    let server = MockServer::start().await;
    let mut settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    let candidates = candidates();
    let advisor = DecisionAdvisor::default();
    settings.mode = DecisionAdvisorMode::Off;
    assert_eq!(
        advisor
            .rank(
                &settings,
                &factory(),
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: "weather",
                    candidates: &candidates,
                    catalog_revision: b"1"
                },
                Some("test")
            )
            .await,
        DecisionAdvice::Fallback(DecisionAdvisorFallback::Disabled)
    );
    settings.mode = DecisionAdvisorMode::Rank;
    let denied = factory().with_network_policy(NetworkPolicyController::default().policy());
    assert_eq!(
        advisor
            .rank(
                &settings,
                &denied,
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: "weather",
                    candidates: &candidates,
                    catalog_revision: b"1"
                },
                Some("test")
            )
            .await,
        DecisionAdvice::Fallback(DecisionAdvisorFallback::NetworkDenied)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn decision_advisor_timeout_falls_back_without_retry() {
    let server = MockServer::start().await;
    let mut settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    settings.timeout = Duration::from_millis(100);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(answer("jev-test"))
                .set_delay(Duration::from_secs(1)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let candidates = candidates();
    let advisor = DecisionAdvisor::default();
    let started = Instant::now();
    let advice = advisor
        .rank(
            &settings,
            &factory(),
            DecisionSearchRequest {
                scope: DecisionSearchScope::Tools,
                query: "weather",
                candidates: &candidates,
                catalog_revision: b"1",
            },
            Some("test"),
        )
        .await;
    assert_eq!(
        advice,
        DecisionAdvice::Fallback(DecisionAdvisorFallback::TimedOut)
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(
        advisor
            .rank(
                &settings,
                &factory(),
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: "weather",
                    candidates: &candidates,
                    catalog_revision: b"1"
                },
                Some("test")
            )
            .await,
        DecisionAdvice::Fallback(DecisionAdvisorFallback::TimedOut)
    );
    assert_eq!(advisor.stats().requests, 1);
}

#[tokio::test]
async fn decision_advisor_cancellation_releases_shared_flight_and_capacity() {
    let server = MockServer::start().await;
    let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(answer("jev-test"))
                .set_delay(Duration::from_millis(70)),
        )
        .expect(2)
        .mount(&server)
        .await;
    let advisor = DecisionAdvisor::default();
    let candidates = candidates();
    let factory = factory();
    let first = advisor.rank(
        &settings,
        &factory,
        DecisionSearchRequest {
            scope: DecisionSearchScope::Tools,
            query: "weather",
            candidates: &candidates,
            catalog_revision: b"1",
        },
        Some("test"),
    );
    tokio::select! {
        _ = first => panic!("response should still be pending"),
        _ = async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        } => {}
    }
    assert_eq!(
        advisor
            .rank(
                &settings,
                &factory,
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: "weather",
                    candidates: &candidates,
                    catalog_revision: b"1"
                },
                Some("test")
            )
            .await,
        DecisionAdvice::Ranked(vec!["t1".into(), "t0".into()])
    );
}

#[tokio::test]
async fn decision_advisor_cache_does_not_cross_credentials_or_catalog_revisions() {
    let server = MockServer::start().await;
    let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer("jev-test")))
        .expect(3)
        .mount(&server)
        .await;
    let advisor = DecisionAdvisor::default();
    let candidates = candidates();
    for (credential, revision) in [("key-a", b"one"), ("key-a", b"two"), ("key-b", b"two")] {
        assert!(matches!(
            advisor
                .rank(
                    &settings,
                    &factory(),
                    DecisionSearchRequest {
                        scope: DecisionSearchScope::Tools,
                        query: "weather",
                        candidates: &candidates,
                        catalog_revision: revision
                    },
                    Some(credential)
                )
                .await,
            DecisionAdvice::Ranked(_)
        ));
    }
    assert_eq!(advisor.stats().requests, 3);
}

#[test]
fn decision_advisor_rejects_invalid_ranking_and_confidence() {
    let settings = DecisionAdvisorSettings {
        model: "jev-test".into(),
        ..Default::default()
    };
    let candidates = candidates();
    let mut response = answer("jev-test");
    response["answers"]["tool"]["probabilities"]["unknown"] = json!(0.1);
    assert_eq!(
        parse_ranking(&settings, &candidates, response),
        Err(DecisionAdvisorFallback::InvalidResponse)
    );
    let mut response = answer("jev-test");
    response["answers"]["tool"]["confidence"] = json!(0.01);
    assert_eq!(
        parse_ranking(&settings, &candidates, response),
        Err(DecisionAdvisorFallback::LowConfidence)
    );
    let mut response = answer("jev-test");
    response["answers"]["tool"]["choice"] = json!("unknown");
    assert_eq!(
        parse_ranking(&settings, &candidates, response),
        Err(DecisionAdvisorFallback::InvalidResponse)
    );
}

#[test]
fn decision_advisor_rejects_remote_http_and_pool_credential_names() {
    let mut settings = DecisionAdvisorSettings {
        mode: DecisionAdvisorMode::Rank,
        ..Default::default()
    };
    for endpoint in [
        "http://example.com/advice",
        "https://token@example.com/advice",
        "https://example.com/advice?api_key=secret",
    ] {
        settings.endpoint = endpoint.into();
        assert!(settings.validate().is_err());
    }
    settings.endpoint = "https://api.typesafe.ai/v1/systemone".into();
    for name in [
        "CODEX_ACCESS_TOKEN",
        "CODEX_API_KEY",
        "OPENAI_API_KEY",
        "CHATGPT_ACCESS_TOKEN",
    ] {
        settings.api_key_env = name.into();
        assert!(settings.validate().is_err());
    }
}

#[tokio::test]
async fn decision_advisor_tool_and_skill_scopes_have_distinct_questions_and_cache_entries() {
    let server = MockServer::start().await;
    let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    let candidates = candidates();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer("jev-test")))
        .expect(2)
        .mount(&server)
        .await;
    let advisor = DecisionAdvisor::default();
    for scope in [DecisionSearchScope::Tools, DecisionSearchScope::Skills] {
        assert!(matches!(
            advisor
                .rank(
                    &settings,
                    &factory(),
                    DecisionSearchRequest {
                        scope,
                        query: "weather",
                        candidates: &candidates,
                        catalog_revision: b"one",
                    },
                    Some("test")
                )
                .await,
            DecisionAdvice::Ranked(_)
        ));
    }
    let requests = server.received_requests().await.unwrap();
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).unwrap())
        .collect::<Vec<_>>();
    assert!(
        bodies[0]["questions"]["tool"]["instructions"]
            .as_str()
            .unwrap()
            .contains("listed tool")
    );
    assert!(
        bodies[1]["questions"]["tool"]["instructions"]
            .as_str()
            .unwrap()
            .contains("listed skill")
    );
    assert_eq!(advisor.stats().requests, 2);
}

#[tokio::test]
async fn decision_advisor_oversized_input_never_leaves_the_host() {
    let server = MockServer::start().await;
    let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    let candidates = candidates();
    let query = "字".repeat(800);
    assert_eq!(
        DecisionAdvisor::default()
            .rank(
                &settings,
                &factory(),
                DecisionSearchRequest {
                    scope: DecisionSearchScope::Tools,
                    query: &query,
                    candidates: &candidates,
                    catalog_revision: b"1"
                },
                Some("test")
            )
            .await,
        DecisionAdvice::Fallback(DecisionAdvisorFallback::OversizedInput)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn decision_advisor_rejects_model_mismatch_and_ambiguous_distribution() {
    let candidates = candidates();
    for provider in [
        DecisionAdvisorProvider::Typesafe,
        DecisionAdvisorProvider::Cloudflare,
    ] {
        let settings = DecisionAdvisorSettings {
            model: "jev-test".into(),
            provider,
            ..Default::default()
        };
        for model in [None, Some("other-model")] {
            let mut body = answer("jev-test");
            match model {
                None => {
                    body.as_object_mut().unwrap().remove("model");
                }
                Some(value) => body["model"] = json!(value),
            };
            let response = if provider == DecisionAdvisorProvider::Cloudflare {
                json!({"success":true,"result":body})
            } else {
                body
            };
            assert_eq!(
                parse_ranking(&settings, &candidates, response),
                Err(DecisionAdvisorFallback::InvalidResponse)
            );
        }
    }
    let settings = DecisionAdvisorSettings {
        model: "jev-test".into(),
        ..Default::default()
    };
    let mut body = answer("jev-test");
    body["answers"]["tool"]["probabilities"] = json!({"t0":0.39,"t1":0.4,"none":0.21});
    assert_eq!(
        parse_ranking(&settings, &candidates, body),
        Err(DecisionAdvisorFallback::LowConfidence)
    );
    let mut body = answer("jev-test");
    body["answers"]["tool"]["choice"] = json!("t0");
    assert_eq!(
        parse_ranking(&settings, &candidates, body),
        Err(DecisionAdvisorFallback::InvalidResponse)
    );
}

#[test]
fn decision_advisor_shadow_comparison_records_no_search_content() {
    let advisor = DecisionAdvisor::default();
    advisor.record_comparison(&[0, 1], &[0, 1]);
    advisor.record_comparison(&[0, 1], &[1, 0]);
    advisor.record_comparison(&[], &[2]);
    assert_eq!(
        advisor.stats(),
        DecisionAdvisorStats {
            comparisons: 3,
            would_change: 2,
            empty_search_recovered: 1,
            ..Default::default()
        }
    );
}
