//! Optional semantic discovery preserves normal tool execution and the advertised catalog.

use anyhow::Result;
use codex_core::StartThreadOptions;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionAdvisorSettings;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use core_test_support::apps_test_server::configure_search_capable_model;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use wiremock::Mock;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

enum AdvisorResponse {
    Valid,
    UnknownId,
}

impl Respond for AdvisorResponse {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("valid advisor body");
        let criteria = body["questions"]["tool"]["criteria"]
            .as_object()
            .expect("choice criteria");
        let target = criteria
            .iter()
            .find(|(_, description)| {
                description
                    .as_str()
                    .is_some_and(|text| text.contains("unique semantic-weather fixture"))
            })
            .map(|(id, _)| id.as_str())
            .expect("weather is reachable through bounded catalog expansion");
        let probabilities = criteria
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    json!(if id == target {
                        0.99
                    } else if id == "none" {
                        0.01
                    } else {
                        0.0
                    }),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
        let choice = match self {
            Self::Valid => target,
            Self::UnknownId => "unregistered_tool",
        };
        ResponseTemplate::new(200).set_body_json(json!({"model":body["model"],"answers":{"tool":{
            "type":"choice","choice":choice,"confidence":0.95,"probabilities":probabilities,
        }}}))
    }
}

async fn discover(
    mode: DecisionAdvisorMode,
    response: AdvisorResponse,
    query: &str,
) -> Result<Value> {
    let server = responses::start_mock_server().await;
    Mock::given(method("POST"))
        .and(path("/decision-fixture"))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    let endpoint = format!("{}/decision-fixture", server.uri());
    let mut builder = test_codex().with_config(move |config| {
        configure_search_capable_model(config);
        config.decision_advisor = DecisionAdvisorSettings {
            mode,
            endpoint,
            api_key_env: String::new(),
            allow_local_http: true,
            ..Default::default()
        };
    });
    let mut test = builder.build_with_auto_env(&server).await?;
    let tools = [
        ("advisor_weather","forecast","Weather forecasting: unique semantic-weather fixture"),
        ("advisor_code","source_search","Find and inspect code repositories"),
    ].into_iter().map(|(name,tool,description)| DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
        name:name.into(), description:description.into(),
        tools:vec![DynamicToolNamespaceTool::Function(DynamicToolFunctionSpec {
            name:tool.into(),description:description.into(),
            input_schema:json!({"type":"object","properties":{},"additionalProperties":false}),
            defer_loading:true,
        })],
    })).collect();
    let thread = test
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: tools,
            environments: Some(vec![test.executor_environment().selection().clone()]),
            ..StartThreadOptions::new(test.config.clone())
        })
        .await?;
    test.codex = thread.thread;
    test.session_configured = thread.session_configured;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_tool_search_call("find-tool", &json!({"query":query,"limit":1})),
                responses::ev_completed("search"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("answer", "done"),
                responses::ev_completed("answer"),
            ]),
        ],
    )
    .await;
    test.submit_turn("Discover a matching tool.").await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    // Advisor results never rewrite or reorder the public tool definitions.
    assert_eq!(
        requests[0].body_json()["tools"],
        requests[1].body_json()["tools"]
    );
    Ok(requests[1].tool_search_output("find-tool"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_advisor_rank_recovers_a_tool_for_a_chinese_query() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let output = discover(
        DecisionAdvisorMode::Rank,
        AdvisorResponse::Valid,
        "查看明天天气预报",
    )
    .await?;
    assert_eq!(output["tools"][0]["name"], json!("advisor_weather"));
    assert_eq!(output["tools"][0]["tools"][0]["name"], json!("forecast"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_advisor_shadow_retains_original_lexical_results() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let output = discover(
        DecisionAdvisorMode::Shadow,
        AdvisorResponse::Valid,
        "advisor_code source_search",
    )
    .await?;
    assert_eq!(output["tools"][0]["name"], json!("advisor_code"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decision_advisor_unknown_ids_fall_back_to_original_search() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let output = discover(
        DecisionAdvisorMode::Rank,
        AdvisorResponse::UnknownId,
        "advisor_code source_search",
    )
    .await?;
    assert_eq!(output["tools"][0]["name"], json!("advisor_code"));
    Ok(())
}
