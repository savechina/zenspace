// 4D Test: AgentExecution, ExecutionMetadata, ToolCall
//
// Dimensions:
//   Normal: Minimal creation, flat token/cost totals
//   Reverse: Empty tool calls, zero costs
//   Adversarial: Overflow costs, many tool calls
//   Logic Tree: Metadata defaults and serde round-trip (006 quality fields)

use zen_agents::{AgentExecution, ExecutionMetadata, ToolCall};
use zen_core::types::Sensitivity;

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn minimal_creates_valid_execution() {
    let exec = AgentExecution::minimal("test-agent", "response text");
    assert_eq!(exec.agent_name, "test-agent");
    assert_eq!(exec.response, "response text");
    assert_eq!(exec.metadata.tokens_used, 0);
    assert_eq!(exec.metadata.cost_estimate, 0.0);
    assert!(exec.tool_calls.is_empty());
}

#[test]
fn total_tokens_flat() {
    let exec = AgentExecution {
        agent_name: "main".into(),
        response: "done".into(),
        metadata: ExecutionMetadata {
            tokens_used: 100,
            cost_estimate: 0.5,
            model_used: "gpt-4".into(),
            duration_ms: 500,
            sensitivity: Sensitivity::Public,
            quality_notes: None,
            delivery_ready: true,
        },
        tool_calls: vec![],
    };
    assert_eq!(exec.total_tokens(), 100);
    assert!((exec.total_cost() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn tool_call_structure() {
    let call = ToolCall {
        tool_name: "search".into(),
        arguments: r#"{"query": "test"}"#.into(),
        result: "found 5 results".into(),
    };
    assert_eq!(call.tool_name, "search");
    assert!(call.arguments.contains("test"));
    assert!(call.result.contains("found"));
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn minimal_with_empty_strings() {
    let exec = AgentExecution::minimal("", "");
    assert_eq!(exec.agent_name, "");
    assert_eq!(exec.response, "");
}

#[test]
fn zero_tokens_and_cost() {
    let exec = AgentExecution::minimal("agent", "ok");
    assert_eq!(exec.total_tokens(), 0);
    assert_eq!(exec.total_cost(), 0.0);
}

#[test]
fn empty_tool_calls_list() {
    let exec = AgentExecution {
        agent_name: "a".into(),
        response: "r".into(),
        metadata: ExecutionMetadata {
            tokens_used: 10,
            cost_estimate: 0.1,
            model_used: "m".into(),
            duration_ms: 100,
            sensitivity: Sensitivity::Private,
            quality_notes: None,
            delivery_ready: true,
        },
        tool_calls: vec![],
    };
    assert_eq!(exec.total_tokens(), 10);
    assert!((exec.total_cost() - 0.1).abs() < f64::EPSILON);
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn very_large_tokens_count() {
    let exec = AgentExecution {
        agent_name: "big".into(),
        response: "x".into(),
        metadata: ExecutionMetadata {
            tokens_used: u32::MAX,
            cost_estimate: f64::MAX,
            model_used: "gpt-4".into(),
            duration_ms: u64::MAX,
            sensitivity: Sensitivity::Confidential,
            quality_notes: None,
            delivery_ready: true,
        },
        tool_calls: vec![],
    };
    assert_eq!(exec.total_tokens(), u32::MAX);
    assert_eq!(exec.total_cost(), f64::MAX);
}

#[test]
fn many_tool_calls() {
    let calls: Vec<ToolCall> = (0..100)
        .map(|i| ToolCall {
            tool_name: format!("tool_{}", i),
            arguments: "{}".into(),
            result: "ok".into(),
        })
        .collect();
    assert_eq!(calls.len(), 100);
    assert_eq!(calls[0].tool_name, "tool_0");
    assert_eq!(calls[99].tool_name, "tool_99");
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn metadata_fields_survive_minimal_construction() {
    let exec = AgentExecution::minimal("agent", "hello");
    assert_eq!(exec.metadata.sensitivity, Sensitivity::Private);
    assert_eq!(exec.metadata.model_used, "");
    assert_eq!(exec.metadata.duration_ms, 0);
}

// 006 US2: quality fields default open — absent JSON fields must decode to
// delivery_ready=true so pre-gate persisted executions stay backward compatible.
#[test]
fn quality_metadata_defaults_and_roundtrip() {
    let exec = AgentExecution::minimal("agent", "hello");
    assert!(exec.metadata.delivery_ready);
    assert!(exec.metadata.quality_notes.is_none());

    let legacy = r#"{
        "agent_name": "agent",
        "response": "r",
        "metadata": {
            "tokens_used": 1,
            "cost_estimate": 0.0,
            "model_used": "",
            "duration_ms": 0,
            "sensitivity": "Public"
        },
        "tool_calls": []
    }"#;
    let decoded: AgentExecution = serde_json::from_str(legacy).expect("legacy payload decodes");
    assert!(decoded.metadata.delivery_ready);
    assert!(decoded.metadata.quality_notes.is_none());

    let encoded = serde_json::to_string(&exec).expect("encodes");
    let decoded: AgentExecution = serde_json::from_str(&encoded).expect("round-trips");
    assert_eq!(decoded.agent_name, "agent");
    assert!(decoded.metadata.delivery_ready);
}
