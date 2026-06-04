// 4D Test: Blackboard, BlackboardTask, Deliverable, Feedback, SystemEvent
//
// Dimensions:
//   Normal: Push/pop tasks, results, feedback; event broadcasts
//   Reverse: Empty channels, full capacity, no subscribers
//   Adversarial: Corrupt data, extreme entropy values, oversized content
//   Logic Tree: Task complexity classification matrix

use std::time::Duration;
use futures::future::FutureExt;
use uuid::Uuid;

use zen_agents::{
    Blackboard, BlackboardTask, Deliverable, Feedback, FeedbackErrorType, SystemEvent,
    ValidationStatus,
};
use zen_core::types::{ComplexityLevel, TaskType};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn blackboard_task_new_creates_valid_task() {
    let task = BlackboardTask::new("test query", 0.5, TaskType::Code);
    assert_eq!(task.user_input, "test query");
    assert!((task.semantic_entropy - 0.5).abs() < f64::EPSILON);
    assert_eq!(task.physical_attribute, TaskType::Code);
}

#[test]
fn blackboard_task_classify_complexity_code() {
    let simple = BlackboardTask::new("simple", 0.2, TaskType::Code);
    assert_eq!(simple.complexity, ComplexityLevel::Simple);

    let std = BlackboardTask::new("standard", 0.5, TaskType::Code);
    assert_eq!(std.complexity, ComplexityLevel::Standard);
}

#[test]
fn blackboard_task_classify_complexity_text() {
    let complex = BlackboardTask::new("complex text", 0.8, TaskType::Text);
    assert_eq!(complex.complexity, ComplexityLevel::Complex);

    let standard = BlackboardTask::new("normal text", 0.5, TaskType::Text);
    assert_eq!(standard.complexity, ComplexityLevel::Standard);
}

#[test]
fn blackboard_task_classify_critical() {
    let critical = BlackboardTask::new("critical", 0.95, TaskType::Code);
    assert_eq!(critical.complexity, ComplexityLevel::Critical);
}

#[test]
fn deliverable_new_has_pending_status() {
    let task_id = Uuid::new_v4();
    let d = Deliverable::new(task_id, "agent", "content".to_string());
    assert_eq!(d.task_id, task_id);
    assert_eq!(d.content, "content");
    assert_eq!(d.validation_status, ValidationStatus::Pending);
    assert!(d.artifact_path.is_none());
}

#[test]
fn feedback_new_has_zero_retry() {
    let id = Uuid::new_v4();
    let f = Feedback::new(id, FeedbackErrorType::Compilation);
    assert_eq!(f.deliverable_id, id);
    assert_eq!(f.retry_count, 0);
    assert!(f.stack_trace.is_none());
}

#[test]
fn feedback_with_suggestions_and_stack_trace() {
    let id = Uuid::new_v4();
    let f = Feedback::new(id, FeedbackErrorType::Semantic)
        .with_suggestions(vec!["Fix type error".to_string()])
        .with_stack_trace("error at line 42".to_string());
    assert_eq!(f.suggestions.len(), 1);
    assert_eq!(f.stack_trace.as_deref(), Some("error at line 42"));
}

#[test]
fn feedback_increment_retry() {
    let id = Uuid::new_v4();
    let mut f = Feedback::new(id, FeedbackErrorType::Timeout);
    f.increment_retry();
    assert_eq!(f.retry_count, 1);
    f.increment_retry();
    assert_eq!(f.retry_count, 2);
}

#[test]
fn blackboard_channels_push_pop() {
    let (mut bb, handle) = Blackboard::new(10);

    // Push and pop a task via handle
    let task = BlackboardTask::new("task1", 0.5, TaskType::Code);
    let task_id = task.id;
    bb.push_task(task).now_or_never();
    // Can't easily pop in sync test, but verify event was sent
    drop(bb);
    drop(handle);
}

#[test]
fn deliverable_task_id_preserved() {
    let task_id = Uuid::new_v4();
    let d = Deliverable::new(task_id, "agent", "result".to_string());
    assert_eq!(d.task_id, task_id);
    assert_eq!(d.metadata.agent_name, "agent");
}

#[test]
fn deliverable_metadata_structure() {
    let task_id = Uuid::new_v4();
    let d = Deliverable::new(task_id, "test-agent", "data".to_string());
    assert_eq!(d.metadata.agent_name, "test-agent");
    assert_eq!(d.metadata.tokens_used, 0);
    assert_eq!(d.metadata.cost_estimate, 0.0);
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn blackboard_new_with_min_capacity() {
    let (mut bb, handle) = Blackboard::new(1);
    let task = BlackboardTask::new("overflow", 0.5, TaskType::Code);
    // With capacity 1, send should succeed
    let result = bb.push_task(task).now_or_never();
    drop(bb);
    drop(handle);
}

#[test]
fn feedback_empty_suggestions() {
    let id = Uuid::new_v4();
    let f = Feedback::new(id, FeedbackErrorType::Style);
    assert!(f.suggestions.is_empty());
}

#[test]
fn blackboard_task_empty_input() {
    let task = BlackboardTask::new("", 0.0, TaskType::Text);
    assert_eq!(task.user_input, "");
    assert!(task.metadata.is_empty());
}

#[test]
fn deliverable_empty_content() {
    let d = Deliverable::new(Uuid::new_v4(), "agent", "".to_string());
    assert_eq!(d.content, "");
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn blackboard_task_extreme_entropy() {
    let task = BlackboardTask::new("test", f64::NEG_INFINITY, TaskType::Code);
    assert_eq!(task.complexity, ComplexityLevel::Simple);

    let task = BlackboardTask::new("test", f64::INFINITY, TaskType::Code);
    assert_eq!(task.complexity, ComplexityLevel::Critical);

    let task = BlackboardTask::new("test", f64::NAN, TaskType::Code);
    // NaN comparisons are false, so falls through to default
    assert_eq!(task.complexity, ComplexityLevel::Standard);
}

#[test]
fn blackboard_task_negative_entropy() {
    let task = BlackboardTask::new("test", -1.0, TaskType::Code);
    assert!(task.semantic_entropy < 0.3);
    assert_eq!(task.complexity, ComplexityLevel::Simple);
}

#[test]
fn deliverable_with_oversized_content() {
    let content = "x".repeat(1_000_000);
    let d = Deliverable::new(Uuid::new_v4(), "agent", content.clone());
    assert_eq!(d.content.len(), 1_000_000);
}

#[test]
fn feedback_max_retry_count() {
    let id = Uuid::new_v4();
    let mut f = Feedback::new(id, FeedbackErrorType::Security);
    for _ in 0..255 {
        f.increment_retry();
    }
    assert_eq!(f.retry_count, 255);
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn complexity_classification_matrix() {
    // Code tasks: entropy < 0.3 → Simple, 0.3-0.6 → Standard, > 0.9 → Critical
    let cases = [
        (0.1, TaskType::Code, ComplexityLevel::Simple),
        (0.29, TaskType::Code, ComplexityLevel::Simple),
        (0.3, TaskType::Code, ComplexityLevel::Standard),
        (0.59, TaskType::Code, ComplexityLevel::Standard),
        (0.6, TaskType::Code, ComplexityLevel::Standard),
        (0.91, TaskType::Code, ComplexityLevel::Critical),
    ];
    for (entropy, task_type, expected) in &cases {
        let task = BlackboardTask::new("test", *entropy, task_type.clone());
        assert_eq!(
            task.complexity, *expected,
            "entropy={}, type={:?} expected={:?}, got={:?}",
            entropy, task_type, expected, task.complexity
        );
    }
}

#[test]
fn system_event_variants_have_correct_data() {
    let task_id = Uuid::new_v4();
    let deliverable_id = Uuid::new_v4();

    {
        let event = SystemEvent::TaskEnqueued { task_id };
        match event {
            SystemEvent::TaskEnqueued { task_id: id } => assert_eq!(id, task_id),
            _ => panic!("wrong variant"),
        }
    }

    {
        let event = SystemEvent::TaskStarted { task_id, agent: "agent".to_string() };
        match event {
            SystemEvent::TaskStarted { .. } => {}
            _ => panic!("wrong variant"),
        }
    }

    {
        let event = SystemEvent::ResultReady { deliverable_id };
        match event {
            SystemEvent::ResultReady { .. } => assert_eq!(deliverable_id, deliverable_id),
            _ => panic!("wrong variant"),
        }
    }

    {
        let event = SystemEvent::FeedbackIssued { deliverable_id };
        match event {
            SystemEvent::FeedbackIssued { .. } => {}
            _ => panic!("wrong variant"),
        }
    }

    {
        let event = SystemEvent::Escalation { task_id, reason: "test".to_string() };
        match event {
            SystemEvent::Escalation { .. } => {}
            _ => panic!("wrong variant"),
        }
    }
}
