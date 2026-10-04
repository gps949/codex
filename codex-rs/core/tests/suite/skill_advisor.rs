//! Skill advice remains bounded contextual guidance, never an implicit skill invocation.

use anyhow::Result;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_model_provider::DecisionAdvisorMode;
use codex_model_provider::DecisionAdvisorSettings;
use core_test_support::responses;
use core_test_support::responses::ResponsesRequest;
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

const SKILL_BODY: &str = "private skill body must-never-auto-load";

#[derive(Clone, Copy, Debug)]
enum Case {
    Rank,
    Shadow,
    NoMatch,
    Off,
    SuggestionsOff,
    Explicit,
    Disabled,
    ExplicitOnly,
}

impl Respond for Case {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value =
            serde_json::from_slice(&request.body).expect("skill advisor fixture should be valid");
        let instructions = body["questions"]["tool"]["instructions"]
            .as_str()
            .expect("skill advisor fixture should be valid");
        assert!(instructions.contains("listed skill"));
        let criteria = body["questions"]["tool"]["criteria"]
            .as_object()
            .expect("skill advisor fixture should be valid");
        let target = criteria
            .iter()
            .find(|(_, description)| {
                description.as_str().is_some_and(|description| {
                    description.contains("unique-decision-report-fixture")
                })
            })
            .map(|(id, _)| id.as_str())
            .expect("skill advisor fixture should be valid");
        let choice = match self {
            Self::NoMatch => "none",
            _ => target,
        };
        let probabilities = criteria
            .keys()
            .map(|id| (id.clone(), json!(if id == choice { 1.0 } else { 0.0 })))
            .collect::<serde_json::Map<String, Value>>();
        assert!(!String::from_utf8_lossy(&request.body).contains(SKILL_BODY));
        assert!(!String::from_utf8_lossy(&request.body).contains("SKILL.md"));
        ResponseTemplate::new(200).set_body_json(json!({"model":body["model"],"answers":{"tool":{
            "type":"choice","choice":choice,"confidence":1.0,"probabilities":probabilities,
        }}}))
    }
}

async fn run(case: Case, turns: &[&str]) -> Result<Vec<ResponsesRequest>> {
    let server = responses::start_mock_server().await;
    let calls = match case {
        Case::Rank | Case::Shadow | Case::NoMatch => 1,
        _ => 0,
    };
    Mock::given(method("POST"))
        .and(path("/skill-fixture"))
        .respond_with(case)
        .expect(calls)
        .mount(&server)
        .await;
    let endpoint = format!("{}/skill-fixture", server.uri());
    let mut builder = test_codex().with_pre_build_hook(move |home| {
        let directory = home.join("skills/decision-report");
        std::fs::create_dir_all(&directory).expect("skill advisor fixture should be valid");
        std::fs::write(directory.join("SKILL.md"),format!("---\nname: decision-report\ndescription: Create a financial chart report, unique-decision-report-fixture.\n---\n{SKILL_BODY}\n")).expect("skill advisor fixture should be valid");
        if matches!(case,Case::ExplicitOnly) {
            std::fs::create_dir_all(directory.join("agents")).expect("skill advisor fixture should be valid");
            std::fs::write(directory.join("agents/openai.yaml"),"policy:\n  allow_implicit_invocation: false\n").expect("skill advisor fixture should be valid");
        }
    }).with_config(move |config| {
        // A User layer also discovers real $HOME/.agents/skills. Isolate the catalog with
        // the same temporary System layer used by the existing skills-extension tests.
        let mut layers = vec![ConfigLayerEntry::new(
            ConfigLayerSource::System { file:config.codex_home.join("config.toml") },
            toml::toml! { skills = { bundled = { enabled = false } } }.into(),
        )];
        if matches!(case,Case::Disabled) {
            layers.push(ConfigLayerEntry::new(ConfigLayerSource::SessionFlags,
                toml::from_str::<toml::Value>("[[skills.config]]\nname = 'decision-report'\nenabled = false\n").expect("skill advisor fixture should be valid"),
            ));
        }
        config.config_layer_stack = ConfigLayerStack::new(layers,ConfigRequirements::default(),ConfigRequirementsToml::default()).expect("skill advisor fixture should be valid");
        config.decision_advisor = DecisionAdvisorSettings {
            mode:match case { Case::Off => DecisionAdvisorMode::Off, Case::Shadow => DecisionAdvisorMode::Shadow, _ => DecisionAdvisorMode::Rank },
            suggest_skills:!matches!(case,Case::SuggestionsOff),
            endpoint,api_key_env:String::new(),allow_local_http:true,
            ..Default::default()
        };
    });
    let test = builder.build_with_auto_env(&server).await?;
    let sequences = turns
        .iter()
        .enumerate()
        .map(|(index, _)| {
            responses::sse(vec![
                responses::ev_assistant_message(&format!("message-{index}"), "done"),
                responses::ev_completed(&format!("response-{index}")),
            ])
        })
        .collect();
    let mock = responses::mount_sse_sequence(&server, sequences).await;
    for turn in turns {
        test.submit_turn(turn).await?;
    }
    let requests = mock.requests();
    let advisor_requests = server
        .received_requests()
        .await
        .expect("skill advisor fixture should be valid")
        .into_iter()
        .filter(|request| request.url.path() == "/skill-fixture")
        .collect::<Vec<_>>();
    assert_eq!(advisor_requests.len(), calls as usize, "case: {case:?}");
    if let Some(request) = advisor_requests.first() {
        let body: Value = serde_json::from_slice(&request.body)?;
        assert_eq!(body["state"]["query"], json!(turns[0]));
    }
    Ok(requests)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_advisor_rank_appends_one_bounded_hint_without_loading_or_rewriting_history()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let requests = run(
        Case::Rank,
        &[
            "帮我制作财务图表报告",
            "Now explicitly use $decision-report.",
        ],
    )
    .await?;
    assert_eq!(requests.len(), 2);
    let first = requests[0].input();
    let next = requests[1].input();
    assert!(next.starts_with(&first));
    assert!(requests[0].has_content_kinds(&["skills.advisor_suggestions"]));
    let hints = requests[0]
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with("<skill_suggestions>"))
        .collect::<Vec<_>>();
    assert_eq!(hints.len(), 1);
    assert!(hints[0].contains("decision-report") && hints[0].len() <= 1200);
    assert!(
        !requests[0]
            .message_input_texts("user")
            .join("\n")
            .contains(SKILL_BODY)
    );
    assert!(requests[1].has_content_kinds(&["skills.selected_skill_instructions"]));
    assert_eq!(
        requests[1]
            .message_input_texts("user")
            .join("\n")
            .matches("<skill_suggestions>")
            .count(),
        1
    );
    let catalogs = requests
        .iter()
        .map(|request| {
            request
                .message_input_texts("developer")
                .into_iter()
                .filter(|text| text.contains("### Available skills"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(catalogs[0], catalogs[1]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_advisor_shadow_and_no_match_preserve_the_original_turn_context() -> Result<()> {
    skip_if_no_network!(Ok(()));
    for case in [Case::Shadow, Case::NoMatch] {
        let requests = run(case, &["Create a financial report."]).await?;
        assert!(!requests[0].has_content_kinds(&["skills.advisor_suggestions"]));
        assert!(
            !requests[0]
                .message_input_texts("user")
                .join("\n")
                .contains(SKILL_BODY)
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_advisor_off_default_explicit_and_disabled_skills_send_no_requests() -> Result<()> {
    skip_if_no_network!(Ok(()));
    for case in [
        Case::Off,
        Case::SuggestionsOff,
        Case::Explicit,
        Case::Disabled,
        Case::ExplicitOnly,
    ] {
        let input = if matches!(case, Case::Explicit) {
            "Use $decision-report."
        } else {
            "Create a financial report."
        };
        let requests = run(case, &[input]).await?;
        assert!(!requests[0].has_content_kinds(&["skills.advisor_suggestions"]));
    }
    Ok(())
}
