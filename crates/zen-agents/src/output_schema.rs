use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

use serde_json::Value;

const MAX_OUTPUT_SCHEMA_RETRIES: usize = 2;

/// Validation error returned when an agent's JSON output does not conform
/// to its declared output schema.
///
/// Contains actionable detail: the agent name, the list of schema
/// violations, and the raw output that failed validation — enough context
/// for a corrective retry prompt.
#[derive(Debug)]
pub struct OutputValidationError {
    agent: String,
    violations: Vec<String>,
    raw_output: String,
}

impl std::error::Error for OutputValidationError {}

impl OutputValidationError {
    pub fn agent(&self) -> &str {
        &self.agent
    }

    pub fn violations(&self) -> &[String] {
        &self.violations
    }

    pub fn raw_output(&self) -> &str {
        &self.raw_output
    }

    fn violation_count(&self) -> usize {
        self.violations.len()
    }
}

/// Result of validating an agent's JSON output against its schema.
pub type OutputValidationResult = Result<(), OutputValidationError>;

fn build_sisyphus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["agent", "blast_radius", "action"],
        "properties": {
            "agent": { "type": "string", "minLength": 1 },
            "blast_radius": { "type": "string", "enum": ["LOW", "MEDIUM", "HIGH"] },
            "action": { "type": "string", "enum": ["route", "delegate", "escalate"] }
        },
        "additionalProperties": false
    })
}

fn build_junior_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["status", "files_changed", "summary"],
        "properties": {
            "status": { "type": "string", "enum": ["ok", "error"] },
            "files_changed": { "type": "array", "items": { "type": "string" } },
            "summary": { "type": "string", "minLength": 1 }
        },
        "additionalProperties": false
    })
}

fn build_hermes_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["gates_pass", "tests_pass", "lint_pass", "ready"],
        "properties": {
            "gates_pass": { "type": "boolean" },
            "tests_pass": { "type": "boolean" },
            "lint_pass": { "type": "boolean" },
            "ready": { "type": "boolean" }
        },
        "additionalProperties": false
    })
}

fn build_metis_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["completeness", "gaps", "recommendations", "architecture_score"],
        "properties": {
            "completeness": { "type": "string" },
            "gaps": { "type": "array", "items": { "type": "string" } },
            "recommendations": { "type": "array", "items": { "type": "string" } },
            "architecture_score": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn build_momus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["pass", "quality_score", "security_score", "issues", "escalate"],
        "properties": {
            "pass": { "type": "boolean" },
            "quality_score": { "type": "string" },
            "security_score": { "type": "string" },
            "issues": { "type": "array", "items": { "type": "string" } },
            "escalate": { "type": "boolean" }
        },
        "additionalProperties": false
    })
}

fn build_oracle_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["question", "analysis", "alternatives", "recommendation", "confidence"],
        "properties": {
            "question": { "type": "string" },
            "analysis": { "type": "string" },
            "alternatives": { "type": "array", "items": { "type": "string" } },
            "recommendation": { "type": "string" },
            "confidence": { "type": "string", "enum": ["high", "medium", "low"] }
        },
        "additionalProperties": false
    })
}

fn build_prometheus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["scope", "tasks", "risk_level"],
        "properties": {
            "scope": { "type": "string" },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["id", "description"],
                    "properties": {
                        "id": { "type": "integer" },
                        "description": { "type": "string" },
                        "depends_on": { "type": "array", "items": { "type": "integer" } }
                    },
                    "additionalProperties": false
                }
            },
            "risk_level": { "type": "string", "enum": ["LOW", "MED", "HIGH"] }
        },
        "additionalProperties": false
    })
}

fn build_explore_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["topic", "findings", "sources", "summary"],
        "properties": {
            "topic": { "type": "string" },
            "findings": { "type": "array", "items": { "type": "string" } },
            "sources": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["url", "relevance"],
                    "properties": {
                        "url": { "type": "string", "format": "uri" },
                        "relevance": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            },
            "summary": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn build_librarian_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["action", "items_processed", "changes", "knowledge_base_size"],
        "properties": {
            "action": { "type": "string", "enum": ["index", "link", "categorize", "summarize"] },
            "items_processed": { "type": "integer", "minimum": 0 },
            "changes": { "type": "array", "items": { "type": "string" } },
            "knowledge_base_size": { "type": "integer", "minimum": 0 }
        },
        "additionalProperties": false
    })
}

fn build_argus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["content_type", "elements", "text_found", "interpretation"],
        "properties": {
            "content_type": { "type": "string", "enum": ["image", "diagram", "screenshot"] },
            "elements": { "type": "array", "items": { "type": "string" } },
            "text_found": { "type": ["string", "null"] },
            "interpretation": { "type": "string" }
        },
        "additionalProperties": false
    })
}

fn build_hephaestus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["scope", "files_changed", "tests_added", "tests_pass", "refactoring_notes"],
        "properties": {
            "scope": { "type": "string" },
            "files_changed": { "type": "array", "items": { "type": "string" } },
            "tests_added": { "type": "integer", "minimum": 0 },
            "tests_pass": { "type": "boolean" },
            "refactoring_notes": { "type": ["string", "null"] }
        },
        "additionalProperties": false
    })
}

fn build_atlas_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["total_units", "completed", "failed", "details"],
        "properties": {
            "total_units": { "type": "integer", "minimum": 0 },
            "completed": { "type": "integer", "minimum": 0 },
            "failed": { "type": "integer", "minimum": 0 },
            "details": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["unit", "status"],
                    "properties": {
                        "unit": { "type": "string" },
                        "status": { "type": "string", "enum": ["ok", "error"] }
                    },
                    "additionalProperties": false
                }
            }
        },
        "additionalProperties": false
    })
}

fn build_zeus_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "required": ["decision", "rationale", "constitutional_basis"],
        "properties": {
            "decision": { "type": "string", "enum": ["approve", "reject", "amnesty", "escalate-to-user"] },
            "rationale": { "type": "string" },
            "constitutional_basis": { "type": "string" }
        },
        "additionalProperties": false
    })
}

static AGENT_SCHEMAS: LazyLock<HashMap<&'static str, Value>> = LazyLock::new(|| {
    let mut m: HashMap<&'static str, Value> = HashMap::new();
    m.insert("sisyphus", build_sisyphus_schema());
    m.insert("junior", build_junior_schema());
    m.insert("hermes", build_hermes_schema());
    m.insert("metis", build_metis_schema());
    m.insert("momus", build_momus_schema());
    m.insert("oracle", build_oracle_schema());
    m.insert("prometheus", build_prometheus_schema());
    m.insert("explore", build_explore_schema());
    m.insert("librarian", build_librarian_schema());
    m.insert("argus", build_argus_schema());
    m.insert("hephaestus", build_hephaestus_schema());
    m.insert("atlas", build_atlas_schema());
    m.insert("zeus", build_zeus_schema());
    m
});

/// Returns the JSON Schema [`Value`] for a builtin agent's expected output,
/// or `None` if the agent has no declared output contract.
///
/// The returned value is a JSON Schema Draft 2020-12 object suitable for
/// passing to [`jsonschema::validate`] or converting to a [`schemars::Schema`]
/// for the `CompletionRequest::output_schema` field.
pub fn agent_output_schema_value(agent_name: &str) -> Option<&'static Value> {
    AGENT_SCHEMAS.get(agent_name)
}

/// Returns a [`schemars::Schema`] wrapping the agent's JSON Schema, suitable
/// for wiring into `CompletionRequest::output_schema`.
///
/// Returns `None` when the agent has no declared output contract.
pub fn agent_output_schema(agent_name: &str) -> Option<schemars::Schema> {
    let value = AGENT_SCHEMAS.get(agent_name)?;
    let value_clone = value.clone();
    schemars::Schema::try_from(value_clone).ok()
}

/// Validate a raw JSON string against the declared output schema for the
/// given agent.
///
/// # Behavior
///
/// - Agents with no schema (e.g., ad-hoc agents) pass unconditionally.
/// - JSON parse failures are reported as a single violation.
/// - Schema compilation failures are reported as a single violation.
/// - All schema validation errors are collected (not just the first).
///
/// # Errors
///
/// Returns [`OutputValidationError`] when the output does not conform.
pub fn validate_output(agent_name: &str, raw_output: &str) -> OutputValidationResult {
    let schema_value = match AGENT_SCHEMAS.get(agent_name) {
        Some(v) => v,
        None => return Ok(()),
    };

    let instance: Value = serde_json::from_str(raw_output).map_err(|e| OutputValidationError {
        agent: agent_name.to_string(),
        violations: vec![format!("invalid JSON: {e}")],
        raw_output: raw_output.to_string(),
    })?;

    let validator = jsonschema::validator_for(schema_value).map_err(|e| OutputValidationError {
        agent: agent_name.to_string(),
        violations: vec![format!("schema compilation failed: {e}")],
        raw_output: raw_output.to_string(),
    })?;

    let violations: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| {
            let path = &e.instance_path;
            if path.as_str().is_empty() {
                format!("{e}")
            } else {
                format!("at `{path}`: {e}")
            }
        })
        .collect();

    if violations.is_empty() {
        Ok(())
    } else {
        Err(OutputValidationError {
            agent: agent_name.to_string(),
            violations,
            raw_output: raw_output.to_string(),
        })
    }
}

/// Maximum number of validation retries before accepting non-conforming output.
pub fn max_output_schema_retries() -> usize {
    MAX_OUTPUT_SCHEMA_RETRIES
}

/// Build a corrective retry prompt that describes the schema violations.
///
/// The prompt is injected into the next LLM turn so the model can
/// self-correct its output structure. Includes the violation details
/// and the original output for reference.
pub fn build_retry_prompt(agent_name: &str, error: &OutputValidationError) -> String {
    let violations_text = error
        .violations()
        .iter()
        .enumerate()
        .map(|(i, v)| format!("  {}. {}", i + 1, v))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Your previous output for agent `{agent_name}` did not conform to the required schema.\n\n\
         Schema violations:\n{violations_text}\n\n\
         Please fix your output to match the expected JSON structure exactly. \
         Return ONLY the corrected JSON, no markdown fences or commentary."
    )
}

/// Append a schema non-conformance record to the audit log.
///
/// Creates `logs/audit.jsonl` if absent. Each record is a single JSON line
/// with kind `agent.output_schema.nonconformance`.
pub fn append_audit_record(
    paths: &std::path::Path,
    agent_name: &str,
    error: &OutputValidationError,
    attempt: usize,
) {
    use std::io::Write;

    let record = serde_json::json!({
        "kind": "agent.output_schema.nonconformance",
        "agent": agent_name,
        "attempt": attempt,
        "max_retries": MAX_OUTPUT_SCHEMA_RETRIES,
        "violation_count": error.violation_count(),
        "violations": error.violations(),
    });

    let audit_dir = paths.join("logs");
    let _ = std::fs::create_dir_all(&audit_dir);
    let audit_path = audit_dir.join("audit.jsonl");

    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_path)
    {
        let _ = writeln!(f, "{record}");
    }
}

impl fmt::Display for OutputValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "agent `{}` output failed schema validation ({} violation(s)): {}",
            self.agent,
            self.violations.len(),
            self.violations.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sisyphus_conforming_output_passes() {
        let output = json!({
            "agent": "Hephaestus",
            "blast_radius": "LOW",
            "action": "route"
        });
        assert!(validate_output("sisyphus", &output.to_string()).is_ok());
    }

    #[test]
    fn sisyphus_missing_required_field_fails() {
        let output = json!({
            "agent": "Hephaestus"
        });
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        assert!(
            err.violations().iter().any(|v| v.contains("blast_radius")),
            "expected blast_radius violation, got: {:?}",
            err.violations()
        );
    }

    #[test]
    fn sisyphus_invalid_enum_fails() {
        let output = json!({
            "agent": "Hephaestus",
            "blast_radius": "EXTREME",
            "action": "route"
        });
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        assert!(
            err.violations().iter().any(|v| v.contains("blast_radius")),
            "expected blast_radius violation, got: {:?}",
            err.violations()
        );
    }

    #[test]
    fn sisyphus_additional_properties_rejected() {
        let output = json!({
            "agent": "Hephaestus",
            "blast_radius": "LOW",
            "action": "route",
            "extra_field": "should not be here"
        });
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        assert!(
            err.violations()
                .iter()
                .any(|v| v.contains("extra_field") || v.contains("additional")),
            "expected additionalProperties violation, got: {:?}",
            err.violations()
        );
    }

    #[test]
    fn junior_conforming_output_passes() {
        let output = json!({
            "status": "ok",
            "files_changed": ["src/main.rs"],
            "summary": "Fixed the bug"
        });
        assert!(validate_output("junior", &output.to_string()).is_ok());
    }

    #[test]
    fn junior_invalid_status_enum_fails() {
        let output = json!({
            "status": "pending",
            "files_changed": [],
            "summary": "Working on it"
        });
        let err = validate_output("junior", &output.to_string()).unwrap_err();
        assert!(
            err.violations().iter().any(|v| v.contains("status")),
            "expected status violation, got: {:?}",
            err.violations()
        );
    }

    #[test]
    fn hermes_conforming_output_passes() {
        let output = json!({
            "gates_pass": true,
            "tests_pass": true,
            "lint_pass": true,
            "ready": true
        });
        assert!(validate_output("hermes", &output.to_string()).is_ok());
    }

    #[test]
    fn oracle_conforming_output_passes() {
        let output = json!({
            "question": "Should we use async?",
            "analysis": "Yes, for I/O bound work.",
            "alternatives": ["sync with threads"],
            "recommendation": "Use async",
            "confidence": "high"
        });
        assert!(validate_output("oracle", &output.to_string()).is_ok());
    }

    #[test]
    fn prometheus_conforming_output_passes() {
        let output = json!({
            "scope": "Add feature X",
            "tasks": [
                {"id": 1, "description": "Design schema"},
                {"id": 2, "description": "Implement", "depends_on": [1]}
            ],
            "risk_level": "LOW"
        });
        assert!(validate_output("prometheus", &output.to_string()).is_ok());
    }

    #[test]
    fn zeus_conforming_output_passes() {
        let output = json!({
            "decision": "approve",
            "rationale": "Meets all criteria",
            "constitutional_basis": "Principle VII"
        });
        assert!(validate_output("zeus", &output.to_string()).is_ok());
    }

    #[test]
    fn atlas_conforming_output_passes() {
        let output = json!({
            "total_units": 3,
            "completed": 2,
            "failed": 1,
            "details": [
                {"unit": "task-1", "status": "ok"},
                {"unit": "task-2", "status": "ok"},
                {"unit": "task-3", "status": "error"}
            ]
        });
        assert!(validate_output("atlas", &output.to_string()).is_ok());
    }

    #[test]
    fn librarian_conforming_output_passes() {
        let output = json!({
            "action": "index",
            "items_processed": 42,
            "changes": ["linked page A to B"],
            "knowledge_base_size": 150
        });
        assert!(validate_output("librarian", &output.to_string()).is_ok());
    }

    #[test]
    fn argus_conforming_output_passes() {
        let output = json!({
            "content_type": "screenshot",
            "elements": ["header", "sidebar", "main"],
            "text_found": "Hello World",
            "interpretation": "A web page"
        });
        assert!(validate_output("argus", &output.to_string()).is_ok());
    }

    #[test]
    fn hephaestus_conforming_output_passes() {
        let output = json!({
            "scope": "Refactor auth module",
            "files_changed": ["src/auth.rs", "src/auth_test.rs"],
            "tests_added": 5,
            "tests_pass": true,
            "refactoring_notes": null
        });
        assert!(validate_output("hephaestus", &output.to_string()).is_ok());
    }

    #[test]
    fn metis_conforming_output_passes() {
        let output = json!({
            "completeness": "8/10",
            "gaps": ["Missing error handling"],
            "recommendations": ["Add retry logic"],
            "architecture_score": "7"
        });
        assert!(validate_output("metis", &output.to_string()).is_ok());
    }

    #[test]
    fn momus_conforming_output_passes() {
        let output = json!({
            "pass": true,
            "quality_score": "8",
            "security_score": "9",
            "issues": [],
            "escalate": false
        });
        assert!(validate_output("momus", &output.to_string()).is_ok());
    }

    #[test]
    fn explore_conforming_output_passes() {
        let output = json!({
            "topic": "Rust async runtimes",
            "findings": ["tokio is dominant"],
            "sources": [{"url": "https://tokio.rs", "relevance": "official docs"}],
            "summary": "tokio is the standard"
        });
        assert!(validate_output("explore", &output.to_string()).is_ok());
    }

    #[test]
    fn unknown_agent_passes_unconditionally() {
        let output = r#"{"anything": "goes"}"#;
        assert!(validate_output("unknown-agent", output).is_ok());
    }

    #[test]
    fn invalid_json_fails() {
        let err = validate_output("sisyphus", "not json at all").unwrap_err();
        assert_eq!(err.violations().len(), 1);
        assert!(err.violations()[0].contains("invalid JSON"));
    }

    #[test]
    fn retry_prompt_includes_violations() {
        let output = json!({"agent": "X"});
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        let prompt = build_retry_prompt("sisyphus", &err);
        assert!(prompt.contains("sisyphus"));
        assert!(prompt.contains("blast_radius"));
        assert!(prompt.contains("violations"));
    }

    #[test]
    fn retry_prompt_includes_raw_output() {
        let raw = r#"{"agent":"X"}"#;
        let err = validate_output("sisyphus", raw).unwrap_err();
        let prompt = build_retry_prompt("sisyphus", &err);
        assert!(prompt.contains("corrected JSON"));
    }

    #[test]
    fn max_retries_is_two() {
        assert_eq!(max_output_schema_retries(), 2);
    }

    #[test]
    fn all_thirteen_agents_have_schemas() {
        let agents = [
            "sisyphus",
            "junior",
            "hermes",
            "metis",
            "momus",
            "oracle",
            "prometheus",
            "explore",
            "librarian",
            "argus",
            "hephaestus",
            "atlas",
            "zeus",
        ];
        for name in agents {
            assert!(
                AGENT_SCHEMAS.contains_key(name),
                "missing schema for builtin agent `{name}`"
            );
        }
        assert_eq!(AGENT_SCHEMAS.len(), 13);
    }

    #[test]
    fn agent_output_schema_returns_schemars_for_known_agent() {
        let schema = agent_output_schema("sisyphus");
        assert!(schema.is_some(), "expected schemars::Schema for sisyphus");
    }

    #[test]
    fn agent_output_schema_returns_none_for_unknown() {
        assert!(agent_output_schema("nonexistent").is_none());
    }

    #[test]
    fn error_display_includes_agent_name() {
        let output = json!({});
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        let display = format!("{err}");
        assert!(display.contains("sisyphus"));
    }

    #[test]
    fn validate_output_multiple_violations_collected() {
        let output = json!({});
        let err = validate_output("sisyphus", &output.to_string()).unwrap_err();
        assert!(
            err.violation_count() >= 3,
            "expected at least 3 violations for empty object, got {}",
            err.violation_count()
        );
    }
}
