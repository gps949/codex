use super::*;
use codex_protocol::protocol::Product;
use codex_protocol::protocol::SkillScope;
use codex_skills::SkillPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

fn skill(home: &std::path::Path, name: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.into(),
        description: format!("Create a {name} report."),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: AbsolutePathBuf::from_absolute_path(home.join(name).join("SKILL.md"))
            .unwrap(),
        scope: SkillScope::Repo,
        plugin_id: None,
        remote_plugin_id: None,
    }
}

#[test]
fn skill_advisor_candidates_respect_disabled_implicit_product_and_name_ambiguity() {
    let home = tempfile::tempdir().unwrap();
    let report = skill(home.path(), "report");
    let disabled = skill(home.path(), "disabled");
    let mut explicit = skill(home.path(), "explicit-only");
    explicit.policy = Some(SkillPolicy {
        allow_implicit_invocation: Some(false),
        products: Vec::new(),
    });
    let mut other_product = skill(home.path(), "other-product");
    other_product.policy = Some(SkillPolicy {
        allow_implicit_invocation: None,
        products: vec![Product::Chatgpt],
    });
    let mut outcome = SkillLoadOutcome::default();
    outcome
        .disabled_paths
        .insert(disabled.path_to_skills_md.clone());
    outcome.skills = vec![
        report,
        disabled,
        explicit,
        other_product,
        skill(home.path(), "duplicate"),
        skill(home.path(), "DUPLICATE"),
    ];
    let (candidates, names, baseline, _) =
        super::candidates(&outcome, &SessionSource::Cli, "Create a report");
    assert_eq!(
        (candidates, names, baseline),
        (
            vec![DecisionCandidate {
                id: "s0".into(),
                description: "report: Create a report report.".into()
            }],
            vec!["report".into()],
            vec![0],
        )
    );
}

#[test]
fn skill_advisor_catalog_expansion_covers_the_last_skill_and_preserves_utf8_limits() {
    let home = tempfile::tempdir().unwrap();
    let mut outcome = SkillLoadOutcome::default();
    outcome.skills = (0..100)
        .map(|index| {
            let mut skill = skill(home.path(), &format!("report-{index}"));
            skill.description = "中文说明".repeat(500);
            skill
        })
        .collect();
    let (candidates, names, _, _) = super::candidates(
        &outcome,
        &SessionSource::Cli,
        "search unmatched foreign words",
    );
    assert!(names.contains(&"report-99".into()));
    assert!(candidates.len() <= 32);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.description.len() <= DECISION_ADVISOR_MAX_DESCRIPTION_BYTES)
    );
}
