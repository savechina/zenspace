use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ZenToolError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    #[error("not found: {0}")]
    NotFound(String),
}

pub type ZenToolResult = Result<Value, ZenToolError>;

pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub args_schema: Value,
    pub result_schema: Value,
}

pub trait ZenTool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn invoke(&self, args: Value) -> impl std::future::Future<Output = ZenToolResult> + Send;
}

fn json_schema_object(props: serde_json::Map<String, Value>, required: Vec<&str>) -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(props));
    if !required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(
                required
                    .into_iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
        );
    }
    Value::Object(schema)
}

pub(crate) fn args_schema_string() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "query".to_string(),
        json_schema_map(&[("type", "string"), ("description", "search query string")]),
    );
    json_schema_object(props, vec!["query"])
}

pub(crate) fn args_schema_string_limit() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "query".to_string(),
        json_schema_map(&[("type", "string"), ("description", "search query string")]),
    );
    props.insert(
        "limit".to_string(),
        json_schema_map(&[
            ("type", "integer"),
            ("description", "maximum number of results"),
        ]),
    );
    json_schema_object(props, vec!["query"])
}

pub(crate) fn args_schema_entity() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "entity_name".to_string(),
        json_schema_map(&[("type", "string"), ("description", "entity name to query")]),
    );
    props.insert(
        "max_depth".to_string(),
        json_schema_map(&[
            ("type", "integer"),
            ("description", "maximum traversal depth"),
        ]),
    );
    json_schema_object(props, vec!["entity_name"])
}

#[allow(dead_code)]
pub(crate) fn args_schema_file_path() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "file_path".to_string(),
        json_schema_map(&[
            ("type", "string"),
            ("description", "path to file for embedding"),
        ]),
    );
    json_schema_object(props, vec!["file_path"])
}

#[allow(dead_code)]
pub(crate) fn args_schema_search_insert() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "id".to_string(),
        json_schema_map(&[("type", "string"), ("description", "note identifier")]),
    );
    props.insert(
        "title".to_string(),
        json_schema_map(&[("type", "string"), ("description", "note title")]),
    );
    props.insert(
        "content".to_string(),
        json_schema_map(&[("type", "string"), ("description", "note content")]),
    );
    props.insert(
        "tags".to_string(),
        json_schema_map(&[("type", "string"), ("description", "comma-separated tags")]),
    );
    props.insert(
        "file_path".to_string(),
        json_schema_map(&[("type", "string"), ("description", "source file path")]),
    );
    props.insert(
        "source".to_string(),
        json_schema_map(&[("type", "string"), ("description", "source of note")]),
    );
    json_schema_object(props, vec!["id", "title", "content", "file_path", "source"])
}

#[allow(dead_code)]
pub(crate) fn args_schema_graph_insert() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "id".to_string(),
        json_schema_map(&[("type", "string"), ("description", "entity identifier")]),
    );
    props.insert(
        "name".to_string(),
        json_schema_map(&[("type", "string"), ("description", "entity display name")]),
    );
    props.insert(
        "entity_type".to_string(),
        json_schema_map(&[("type", "string"), ("description", "type of entity")]),
    );
    json_schema_object(props, vec!["id", "name", "entity_type"])
}

#[allow(dead_code)]
pub(crate) fn args_schema_relationship_insert() -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "id".to_string(),
        json_schema_map(&[
            ("type", "string"),
            ("description", "relationship identifier"),
        ]),
    );
    props.insert(
        "source_id".to_string(),
        json_schema_map(&[("type", "string"), ("description", "source entity id")]),
    );
    props.insert(
        "target_id".to_string(),
        json_schema_map(&[("type", "string"), ("description", "target entity id")]),
    );
    props.insert(
        "relation_type".to_string(),
        json_schema_map(&[("type", "string"), ("description", "type of relationship")]),
    );
    props.insert(
        "confidence".to_string(),
        json_schema_map(&[
            ("type", "number"),
            ("description", "relationship confidence 0-1"),
        ]),
    );
    json_schema_object(props, vec!["id", "source_id", "target_id", "relation_type"])
}

pub(crate) fn result_schema_array() -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), Value::String("array".to_string()));
    schema.insert("items".to_string(), Value::Object(serde_json::Map::new()));
    Value::Object(schema)
}

pub(crate) fn result_schema_string() -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    Value::Object(schema)
}

fn json_schema_map(fields: &[(&str, &str)]) -> Value {
    let props: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect();
    Value::Object(props)
}
