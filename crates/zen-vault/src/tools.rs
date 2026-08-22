use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// A workspace-resolved SQLite client shared by KB-backed agent tools.
///
/// The path is fixed at construction (resolved from `ZenPaths` by the
/// wiring layer); the client itself is opened lazily on first use because
/// `SqliteClient::open` is async while tool construction is sync, and
/// opening runs the migration check — exactly once per shared holder
/// instead of once per invocation.
///
/// Clones share the same underlying client.
#[derive(Clone)]
pub struct SharedSqliteClient {
    db_path: PathBuf,
    client: Arc<tokio::sync::OnceCell<Arc<zen_repo::SqliteClient>>>,
}

impl SharedSqliteClient {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            client: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub async fn get(&self) -> Result<Arc<zen_repo::SqliteClient>, String> {
        self.client
            .get_or_try_init(|| async {
                zen_repo::SqliteClient::open(&self.db_path)
                    .await
                    .map(Arc::new)
                    .map_err(|e| {
                        format!("failed to open state db at {}: {e}", self.db_path.display())
                    })
            })
            .await
            .map_err(|e: String| e)
            .map(Arc::clone)
    }
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
        "notion_name".to_string(),
        json_schema_map(&[("type", "string"), ("description", "notion name to query")]),
    );
    props.insert(
        "max_depth".to_string(),
        json_schema_map(&[
            ("type", "integer"),
            ("description", "maximum traversal depth"),
        ]),
    );
    json_schema_object(props, vec!["notion_name"])
}

// Schema helpers for future tool implementations. Kept as public crate
// API so tool authors don't need to rewrite JSON schema definitions.
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
        json_schema_map(&[("type", "string"), ("description", "notion identifier")]),
    );
    props.insert(
        "name".to_string(),
        json_schema_map(&[("type", "string"), ("description", "notion display name")]),
    );
    props.insert(
        "kind".to_string(),
        json_schema_map(&[("type", "string"), ("description", "type of notion")]),
    );
    json_schema_object(props, vec!["id", "name", "kind"])
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
        json_schema_map(&[("type", "string"), ("description", "source notion id")]),
    );
    props.insert(
        "target_id".to_string(),
        json_schema_map(&[("type", "string"), ("description", "target notion id")]),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_args_schema_file_path() {
        let schema = args_schema_file_path();
        assert!(schema.is_object());
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("file_path"));
    }

    #[test]
    fn test_args_schema_search_insert() {
        let schema = args_schema_search_insert();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("title"));
        assert!(props.contains_key("content"));
    }

    #[test]
    fn test_args_schema_graph_insert() {
        let schema = args_schema_graph_insert();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(props.contains_key("kind"));
    }

    #[test]
    fn test_args_schema_relationship_insert() {
        let schema = args_schema_relationship_insert();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("source_id"));
        assert!(props.contains_key("target_id"));
        assert!(props.contains_key("relation_type"));
    }

    #[test]
    fn test_all_schema_functions_return_valid_json() {
        for (name, schema) in [
            ("file_path", args_schema_file_path()),
            ("search_insert", args_schema_search_insert()),
            ("graph_insert", args_schema_graph_insert()),
            ("relationship_insert", args_schema_relationship_insert()),
        ] {
            assert!(schema.is_object(), "{} should return a JSON object", name);
        }
    }
}
