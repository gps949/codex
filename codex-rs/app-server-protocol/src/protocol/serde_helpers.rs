use std::path::PathBuf;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

#[allow(dead_code)]
pub(crate) fn nullable_string_schema(
    generator: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    generator.subschema_for::<Option<String>>()
}

#[allow(dead_code)]
pub(crate) fn optional_account_pool_read_response_schema(
    _generator: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    use schemars::schema::InstanceType;
    use schemars::schema::Schema;
    use schemars::schema::SchemaObject;
    use schemars::schema::SubschemaValidation;

    Schema::Object(SchemaObject {
        subschemas: Some(Box::new(SubschemaValidation {
            any_of: Some(vec![
                Schema::Object(SchemaObject {
                    instance_type: Some(InstanceType::Null.into()),
                    ..Default::default()
                }),
                Schema::Object(SchemaObject {
                    reference: Some("#/definitions/AccountPoolReadResponse".to_string()),
                    ..Default::default()
                }),
            ]),
            ..Default::default()
        })),
        ..Default::default()
    })
}

pub fn deserialize_empty_path_as_none<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    let path = Option::<PathBuf>::deserialize(deserializer)?;
    Ok(path.filter(|path| !path.as_os_str().is_empty()))
}

pub fn deserialize_double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    serde_with::rust::double_option::deserialize(deserializer)
}

pub fn serialize_double_option<T, S>(
    value: &Option<Option<T>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    serde_with::rust::double_option::serialize(value, serializer)
}
