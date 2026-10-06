use super::*;
use crate::DecisionAdvisorProvider;
use crate::RoutingTaskComplexity;
use codex_http_client::NetworkPolicyController;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn settings(server: &MockServer, provider: DecisionAdvisorProvider) -> DecisionAdvisorSettings {
    DecisionAdvisorSettings {
        mode: DecisionAdvisorMode::Off,
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

fn answer(model: &str, choice: &str) -> Value {
    let mut probabilities = json!({"simple":0.01,"standard":0.01,"demanding":0.01,"high_stakes":0.01,"unknown":0.01,"none":0.01});
    probabilities[choice] = json!(0.95);
    json!({"model":model,"answers":{"task":{
        "type":"choice","choice":choice,"confidence":0.94,"probabilities":probabilities
    }}})
}

fn factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

#[tokio::test]
async fn unavailable_service_cools_different_tasks_but_new_credentials_can_recover() {
    for (status, retry_after) in [(401, ""), (429, "120"), (503, "60")] {
        let server = MockServer::start().await;
        let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
        Mock::given(method("POST"))
            .and(header("Authorization", "Bearer old-test-key"))
            .respond_with(ResponseTemplate::new(status).insert_header("Retry-After", retry_after))
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(header("Authorization", "Bearer new-test-key"))
            .respond_with(
                ResponseTemplate::new(/*s*/ 200).set_body_json(answer(&settings.model, "standard")),
            )
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let advisor = DecisionAdvisor::default();
        let factory = factory();
        for task in ["Implement one feature.", "Investigate another feature."] {
            assert_eq!(
                advisor
                    .assess_task(&settings, &factory, task, Some("old-test-key"))
                    .await,
                Err(DecisionAdvisorFallback::Unavailable)
            );
        }
        assert_eq!(
            advisor
                .assess_task(
                    &settings,
                    &factory,
                    "Implement one feature.",
                    Some("new-test-key")
                )
                .await,
            Ok(RoutingTaskComplexity::Standard)
        );
        assert_eq!(advisor.stats().requests, 2);
    }
}

#[tokio::test]
async fn service_assessment_works_with_tools_off_and_deduplicates_exact_credentials() {
    for provider in [
        DecisionAdvisorProvider::Typesafe,
        DecisionAdvisorProvider::Cloudflare,
    ] {
        let server = MockServer::start().await;
        let settings = settings(&server, provider);
        let body = answer(&settings.model, "demanding");
        let response = match provider {
            DecisionAdvisorProvider::Typesafe => body,
            DecisionAdvisorProvider::Cloudflare => json!({"success":true,"result":body}),
        };
        Mock::given(method("POST"))
            .and(path("/advisor"))
            .and(header("Authorization", "Bearer separate-test-key"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(response))
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        let advisor = DecisionAdvisor::default();
        let factory = factory();
        let task = "Investigate this cross-module race.";
        let (first, second) = tokio::join!(
            advisor.assess_task(&settings, &factory, task, Some("separate-test-key")),
            advisor.assess_task(&settings, &factory, task, Some("separate-test-key")),
        );
        assert_eq!(
            (first, second),
            (
                Ok(RoutingTaskComplexity::Demanding),
                Ok(RoutingTaskComplexity::Demanding)
            )
        );
        assert_eq!(settings.mode, DecisionAdvisorMode::Off);
        assert_eq!(advisor.stats().rankings, 0);
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["state"], json!({"query":task}));
        assert_eq!(body["questions"]["task"]["type"], "choice");
        assert!(body["questions"].get("tool").is_none());
    }
}

#[tokio::test]
async fn service_assessment_validates_even_with_tool_mode_off() {
    let server = MockServer::start().await;
    let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    let advisor = DecisionAdvisor::default();
    let mut invalid = settings.clone();
    invalid.timeout = Duration::from_millis(/*millis*/ 1);
    assert_eq!(
        advisor
            .assess_task(&invalid, &factory(), "Translate this.", Some("test"))
            .await,
        Err(DecisionAdvisorFallback::InvalidConfiguration)
    );
    let denied = factory().with_network_policy(NetworkPolicyController::default().policy());
    assert_eq!(
        advisor
            .assess_task(&settings, &denied, "Translate this.", Some("test"))
            .await,
        Err(DecisionAdvisorFallback::NetworkDenied)
    );
    assert_eq!(
        advisor
            .assess_task(&settings, &factory(), &"a".repeat(/*n*/ 2049), Some("test"))
            .await,
        Err(DecisionAdvisorFallback::OversizedInput)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn service_assessment_rejects_malformed_mismatched_and_ambiguous_answers() {
    for invalid in 0..4 {
        let server = MockServer::start().await;
        let settings = settings(&server, DecisionAdvisorProvider::Typesafe);
        let mut response = answer(&settings.model, "simple");
        let expected = match invalid {
            0 => {
                response["model"] = json!("unexpected");
                DecisionAdvisorFallback::InvalidResponse
            }
            1 => {
                response["answers"]["task"]["choice"] = json!("surprise");
                DecisionAdvisorFallback::InvalidResponse
            }
            2 => {
                response["answers"]["task"]["probabilities"]["simple"] = json!(1.1);
                DecisionAdvisorFallback::InvalidResponse
            }
            3 => {
                response["answers"]["task"]["probabilities"] = json!({"simple":0.2,"standard":0.2,"demanding":0.2,"high_stakes":0.2,"unknown":0.1,"none":0.1});
                DecisionAdvisorFallback::LowConfidence
            }
            _ => unreachable!(),
        };
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(response))
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
        assert_eq!(
            DecisionAdvisor::default()
                .assess_task(&settings, &factory(), "Translate this.", Some("test"))
                .await,
            Err(expected)
        );
    }
}

#[tokio::test]
async fn service_assessment_times_out_without_retrying_the_paid_call() {
    let server = MockServer::start().await;
    let mut settings = settings(&server, DecisionAdvisorProvider::Typesafe);
    settings.timeout = Duration::from_millis(/*millis*/ 100);
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(answer(&settings.model, "simple"))
                .set_delay(Duration::from_secs(/*secs*/ 1)),
        )
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let advisor = DecisionAdvisor::default();
    for _ in 0..2 {
        assert_eq!(
            advisor
                .assess_task(&settings, &factory(), "Translate this.", Some("test"))
                .await,
            Err(DecisionAdvisorFallback::TimedOut)
        );
    }
}
