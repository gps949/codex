//! A response reused by another response must keep the same schema contract.

use super::GeneratedSchema;
use super::build_schema_bundle;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn bundles_a_root_response_reused_as_a_nested_definition() -> Result<()> {
    let response = json!({
        "title": "SharedResponse", "type": "object",
        "properties": {"enabled": {"type": "boolean"}}, "required": ["enabled"]
    });
    let mut root_response = response.clone();
    root_response["$schema"] = json!("http://json-schema.org/draft-07/schema#");
    let schemas = vec![
        GeneratedSchema {
            namespace: Some("v2".into()),
            logical_name: "SharedResponse".into(),
            value: root_response.clone(),
            in_v1_dir: false,
        },
        GeneratedSchema {
            namespace: Some("v2".into()),
            logical_name: "ConsumerResponse".into(),
            value: json!({
                "title": "ConsumerResponse", "type": "object",
                "properties": {"shared": {"$ref": "#/definitions/SharedResponse"}},
                "definitions": {"SharedResponse": response}
            }),
            in_v1_dir: false,
        },
    ];
    let bundle = build_schema_bundle(schemas)?;
    assert_eq!(bundle["definitions"]["v2"]["SharedResponse"], root_response);
    assert_eq!(
        bundle["definitions"]["v2"]["ConsumerResponse"]["properties"],
        json!({"shared": {"$ref": "#/definitions/v2/SharedResponse"}})
    );
    Ok(())
}
