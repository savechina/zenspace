// 4D Test: AgentExecution, ExecutionMetadata, ToolCall
//
// Dimensions:
//   Normal: Minimal creation, recursive totals
//   Reverse: Empty sub-agent results, zero costs
//   Adversarial: Deeply nested sub-agents, overflow costs
//   Logic Tree: Token/cost accumulation across hierarchy

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
    assert!(exec.sub_agent_results.is_empty());
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
        },
        tool_calls: vec![],
        sub_agent_results: vec![],
    };
    assert_eq!(exec.total_tokens(), 100);
    assert!((exec.total_cost() - 0.5).abs() < f64::EPSILON);
}

#[test]
fn total_tokens_with_sub_agents() {
    let sub = AgentExecution::minimal("sub", "sub response");
    let sub_with_tokens = AgentExecution {
        agent_name: "sub".into(),
        response: "sub".into(),
        metadata: ExecutionMetadata {
            tokens_used: 50,
            cost_estimate: 0.25,
            model_used: "gpt-4".into(),
            duration_ms: 200,
            sensitivity: Sensitivity::Public,
        },
        tool_calls: vec![],
        sub_agent_results: vec![sub],
    };

    let main = AgentExecution {
        agent_name: "main".into(),
        response: "main".into(),
        metadata: ExecutionMetadata {
            tokens_used: 200,
            cost_estimate: 1.0,
            model_used: "gpt-4".into(),
            duration_ms: 1000,
            sensitivity: Sensitivity::Public,
        },
        tool_calls: vec![],
        sub_agent_results: vec![sub_with_tokens],
    };

    assert_eq!(main.total_tokens(), 250); // 200 + 50
    assert!((main.total_cost() - 1.25).abs() < f64::EPSILON); // 1.0 + 0.25
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
fn empty_sub_agent_list() {
    let exec = AgentExecution {
        agent_name: "a".into(),
        response: "r".into(),
        metadata: ExecutionMetadata {
            tokens_used: 10,
            cost_estimate: 0.1,
            model_used: "m".into(),
            duration_ms: 100,
            sensitivity: Sensitivity::Private,
        },
        tool_calls: vec![],
        sub_agent_results: vec![],
    };
    assert_eq!(exec.total_tokens(), 10);
    assert!((exec.total_cost() - 0.1).abs() < f64::EPSILON);
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn deeply_nested_sub_agents() {
    let mut innermost = AgentExecution::minimal("depth-10", "leaf");
    innermost.metadata.tokens_used = 1;
    innermost.metadata.cost_estimate = 0.01;

    let mut chain = innermost;
    for i in 0..9 {
        chain = AgentExecution {
            agent_name: format!("depth-{}", 9 - i),
            response: "".into(),
            metadata: ExecutionMetadata {
                tokens_used: 1,
                cost_estimate: 0.01,
                model_used: "m".into(),
                duration_ms: 10,
                sensitivity: Sensitivity::Public,
            },
            tool_calls: vec![],
            sub_agent_results: vec![chain],
        };
    }

    assert_eq!(chain.total_tokens(), 10);
    assert!((chain.total_cost() - 0.10).abs() < f64::EPSILON);
}

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
        },
        tool_calls: vec![],
        sub_agent_results: vec![],
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

#[test]
fn sub_agent_results_do_not_affect_main_agent_name() {
    let sub = AgentExecution::minimal("sub", "response");
    let main = AgentExecution {
        agent_name: "orchestrator".into(),
        response: "done".into(),
        metadata: ExecutionMetadata {
            tokens_used: 10,
            cost_estimate: 0.1,
            model_used: "m".into(),
            duration_ms: 100,
            sensitivity: Sensitivity::Public,
        },
        tool_calls: vec![],
        sub_agent_results: vec![sub],
    };
    assert_eq!(main.agent_name, "orchestrator");
    assert_eq!(main.sub_agent_results[0].agent_name, "sub");
}
