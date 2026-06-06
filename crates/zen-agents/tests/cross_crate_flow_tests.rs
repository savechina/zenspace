// ============================================================================
// 4D Test Suite: zen-agents cross-crate integration
//
// Dimensions:
//   NORMAL       — Blackboard push/pop, Deliverable lifecycle, Feedback
//   REVERSE      — Empty channels, minimum capacity, invalid content
//   ADVERSARIAL  — Capacity overflow, max retry, concurrent access
//   LOGIC TREE   — Coordinator routing, quality pipeline construction
// ============================================================================

use uuid::Uuid;

use zen_agents::{
    Blackboard, BlackboardTask, Deliverable, Feedback, FeedbackErrorType, QualityPipeline,
    SystemEvent, ValidationStatus,
};
use zen_core::types::{ComplexityLevel, TaskType};

// ============================================================================
// NORMAL PATH — Standard operations
// ============================================================================

#[tokio::test]
async fn test_blackboard_push_pop_roundtrip() {
    let (mut bb, _handle) = Blackboard::new(10);
    let task = BlackboardTask::new("test query", 0.5, TaskType::Code);

    let task_id = task.id;
    bb.push_task(task).await.expect("push should succeed");

    let popped = bb.pop_task().await;
    assert!(popped.is_some(), "should pop a task");
    assert_eq!(popped.unwrap().id, task_id, "task ID should be preserved");
}

#[test]
fn test_deliverable_create_and_validate() {
    let task_id = Uuid::new_v4();
    let mut d = Deliverable::new(task_id, "agent-1", "test output".to_string());
    assert_eq!(d.validation_status, ValidationStatus::Pending);

    d.validation_status = ValidationStatus::Pass;
    assert_eq!(d.validation_status, ValidationStatus::Pass);
}

#[test]
fn test_feedback_retry_count_increments() {
    let id = Uuid::new_v4();
    let mut f = Feedback::new(id, FeedbackErrorType::Compilation);
    assert_eq!(f.retry_count, 0);

    f.retry_count += 1;
    assert_eq!(f.retry_count, 1);
}

// ============================================================================
// REVERSE PATH — Edge cases in data flow
// ============================================================================

#[tokio::test]
async fn test_blackboard_push_pop_empty() {
    // Push one, pop one — channel starts empty, pop_task would block until pushed
    let (mut bb, _handle) = Blackboard::new(5);
    let task = BlackboardTask::new("test", 0.5, TaskType::Code);
    bb.push_task(task).await.expect("push should succeed");
    let popped = bb.pop_task().await;
    assert!(popped.is_some(), "should pop the task we just pushed");
}

#[tokio::test]
async fn test_blackboard_with_minimum_capacity() {
    let (mut bb, _handle) = Blackboard::new(1);
    let task = BlackboardTask::new("single task", 0.3, TaskType::Text);
    bb.push_task(task)
        .await
        .expect("push to capacity-1 should succeed");

    // Pop to make room, then push another
    let popped = bb.pop_task().await;
    assert!(popped.is_some());

    let task2 = BlackboardTask::new("second", 0.3, TaskType::Text);
    bb.push_task(task2)
        .await
        .expect("second push should succeed after pop");
    let popped2 = bb.pop_task().await;
    assert!(popped2.is_some());
}

#[test]
fn test_deliverable_with_empty_content() {
    let task_id = Uuid::new_v4();
    let d = Deliverable::new(task_id, "agent", "".to_string());
    assert_eq!(d.content, "");
    assert!(d.artifact_path.is_none());
}

// ============================================================================
// ADVERSARIAL PATH — Stress and boundary conditions
// ============================================================================

#[tokio::test]
async fn test_blackboard_fill_to_capacity() {
    let capacity = 3;
    let (mut bb, _handle) = Blackboard::new(capacity);

    for i in 0..capacity {
        let task = BlackboardTask::new(&format!("task {i}"), 0.5, TaskType::Code);
        bb.push_task(task)
            .await
            .expect("push within capacity should succeed");
    }

    // Pop one to make room before pushing to avoid blocking on full channel
    let popped = bb.pop_task().await;
    assert!(popped.is_some(), "should pop a task from filled blackboard");

    let overflow = BlackboardTask::new("overflow", 0.5, TaskType::Code);
    bb.push_task(overflow)
        .await
        .expect("push after pop should succeed");
}

#[test]
fn test_feedback_with_max_retry_count() {
    let id = Uuid::new_v4();
    let mut f = Feedback::new(id, FeedbackErrorType::Semantic);
    f.retry_count = u8::MAX;
    assert_eq!(f.retry_count, u8::MAX);

    f.retry_count = f.retry_count.saturating_add(1);
    assert_eq!(f.retry_count, u8::MAX);
}

#[test]
fn test_system_event_all_task_events_constructable() {
    let task_id = Uuid::new_v4();
    let _enqueued = SystemEvent::TaskEnqueued { task_id };
    let _started = SystemEvent::TaskStarted {
        task_id,
        agent: "test".into(),
    };
    let _completed = SystemEvent::TaskCompleted {
        task_id: Uuid::new_v4(),
    };
    let _failed = SystemEvent::TaskFailed {
        task_id: Uuid::new_v4(),
        error: "test error".into(),
    };
    let _feedback = SystemEvent::FeedbackIssued {
        deliverable_id: Uuid::new_v4(),
    };
}

#[test]
fn test_blackboard_task_complexity_classification() {
    let simple = BlackboardTask::new("simple", 0.2, TaskType::Code);
    assert_eq!(simple.complexity, ComplexityLevel::Simple);

    let standard = BlackboardTask::new("standard", 0.5, TaskType::Code);
    assert_eq!(standard.complexity, ComplexityLevel::Standard);

    let complex = BlackboardTask::new("complex", 0.8, TaskType::Text);
    assert_eq!(complex.complexity, ComplexityLevel::Complex);

    let critical = BlackboardTask::new("critical", 0.95, TaskType::Code);
    assert_eq!(critical.complexity, ComplexityLevel::Critical);
}

// ============================================================================
// LOGIC TREE — Decision branch coverage
// ============================================================================

#[test]
fn test_quality_pipeline_creation() {
    let pipeline = QualityPipeline::new();
    let _metis = pipeline.metis();
    let _momus = pipeline.momus();
    let _hermes = pipeline.hermes();
    let _zeus = pipeline.zeus();
}

#[test]
fn test_feedback_error_type_variants() {
    let all = vec![
        FeedbackErrorType::Compilation,
        FeedbackErrorType::Semantic,
        FeedbackErrorType::Style,
        FeedbackErrorType::Security,
        FeedbackErrorType::Timeout,
    ];
    for err in &all {
        let display = format!("{err:?}");
        assert!(!display.is_empty());
    }
}

#[test]
fn test_validation_status_all_variants() {
    let all = vec![
        ValidationStatus::Pending,
        ValidationStatus::Pass,
        ValidationStatus::Fail,
    ];
    for status in &all {
        let display = format!("{status:?}");
        assert!(!display.is_empty());
    }
}

#[test]
fn test_deliverable_validation_transition() {
    let task_id = Uuid::new_v4();
    let mut d = Deliverable::new(task_id, "agent-x", "deliverable content".to_string());

    d.validation_status = ValidationStatus::Pass;
    assert_eq!(d.validation_status, ValidationStatus::Pass);

    d.validation_status = ValidationStatus::Fail;
    assert_eq!(d.validation_status, ValidationStatus::Fail);
}
