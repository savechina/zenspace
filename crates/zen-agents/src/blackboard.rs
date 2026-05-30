// T266: tokio Blackboard channel architecture (FR-SO-001, FR-SO-002)
// Multi-producer, multi-consumer channels with backpressure
// broadcast for event fan-out to all agents

use arrow::array::RecordBatch;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use zen_core::types::{ComplexityLevel, TaskType};

type TaskReceiver = Option<mpsc::Receiver<BlackboardTask>>;
type ResultReceiver = Option<mpsc::Receiver<Deliverable>>;
type FeedbackReceiver = Option<mpsc::Receiver<Feedback>>;

// ---------------------------------------------------------------------------
// Core Data Structures (T267, T268)
// ---------------------------------------------------------------------------

/// Structured task with semantic entropy calculation (T267)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardTask {
    pub id: Uuid,
    pub user_input: String,
    pub semantic_entropy: f64,
    pub complexity: ComplexityLevel,
    pub physical_attribute: TaskType,
    pub created_at: DateTime<Utc>,
    pub metadata: std::collections::HashMap<String, String>,
}

impl BlackboardTask {
    pub fn new(user_input: &str, semantic_entropy: f64, task_type: TaskType) -> Self {
        let complexity = Self::classify_complexity(semantic_entropy, &task_type);
        Self {
            id: Uuid::now_v7(),
            user_input: user_input.to_string(),
            semantic_entropy,
            complexity,
            physical_attribute: task_type,
            created_at: Utc::now(),
            metadata: std::collections::HashMap::new(),
        }
    }

    fn classify_complexity(entropy: f64, task_type: &TaskType) -> ComplexityLevel {
        match (entropy, task_type) {
            (e, TaskType::Code) if e < 0.3 => ComplexityLevel::Simple,
            (e, TaskType::Code) if e < 0.6 => ComplexityLevel::Standard,
            (e, TaskType::Text) if e > 0.7 => ComplexityLevel::Complex,
            (e, _) if e > 0.9 => ComplexityLevel::Critical,
            _ => ComplexityLevel::Standard,
        }
    }
}

/// Deliverable with artifact reference (not inline text) (T267)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub task_id: Uuid,
    pub artifact_path: Option<std::path::PathBuf>,
    pub content: String,
    pub metadata: DeliverableMetadata,
    pub validation_status: ValidationStatus,
    #[serde(skip)]
    pub arrow_data: Option<Arc<RecordBatch>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverableMetadata {
    pub agent_name: String,
    pub created_at: DateTime<Utc>,
    pub tokens_used: u64,
    pub cost_estimate: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationStatus {
    Pending,
    Pass,
    Fail,
}

impl Deliverable {
    pub fn new(task_id: Uuid, agent_name: &str, content: String) -> Self {
        Self {
            task_id,
            artifact_path: None,
            content,
            metadata: DeliverableMetadata {
                agent_name: agent_name.to_string(),
                created_at: Utc::now(),
                tokens_used: 0,
                cost_estimate: 0.0,
            },
            validation_status: ValidationStatus::Pending,
            arrow_data: None,
        }
    }

    pub fn with_arrow_data(mut self, data: Arc<RecordBatch>) -> Self {
        self.arrow_data = Some(data);
        self
    }
}

/// Feedback object for Hermes loopback (not just "try again") (T268)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feedback {
    pub deliverable_id: Uuid,
    pub error_type: FeedbackErrorType,
    pub stack_trace: Option<String>,
    pub suggestions: Vec<String>,
    pub retry_count: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FeedbackErrorType {
    Compilation,
    Semantic,
    Style,
    Security,
    Timeout,
}

impl Feedback {
    pub fn new(deliverable_id: Uuid, error_type: FeedbackErrorType) -> Self {
        Self {
            deliverable_id,
            error_type,
            stack_trace: None,
            suggestions: Vec::new(),
            retry_count: 0,
        }
    }

    pub fn with_suggestions(mut self, suggestions: Vec<String>) -> Self {
        self.suggestions = suggestions;
        self
    }

    pub fn with_stack_trace(mut self, trace: String) -> Self {
        self.stack_trace = Some(trace);
        self
    }

    pub fn increment_retry(&mut self) {
        self.retry_count += 1;
    }
}

/// System events for broadcast fan-out (T266)
#[derive(Debug, Clone)]
pub enum SystemEvent {
    TaskEnqueued { task_id: Uuid },
    TaskStarted { task_id: Uuid, agent: String },
    TaskCompleted { task_id: Uuid },
    TaskFailed { task_id: Uuid, error: String },
    ResultReady { deliverable_id: Uuid },
    FeedbackIssued { deliverable_id: Uuid },
    AgentIdle { agent: String },
    AgentBusy { agent: String },
    BudgetThreshold { available: u64 },
    Escalation { task_id: Uuid, reason: String },
}

// ---------------------------------------------------------------------------
// Blackboard Architecture (T266)
// ---------------------------------------------------------------------------

/// The Blackboard is the central communication hub for all agents
pub struct Blackboard {
    // Task queue: Sisyphus pushes, Prometheus/Atlas consume
    task_tx: mpsc::Sender<BlackboardTask>,
    task_rx: Option<mpsc::Receiver<BlackboardTask>>,

    // Event bus: all agents subscribe to system events
    event_tx: broadcast::Sender<SystemEvent>,

    // Result queue: workers push, Hermes consumes
    result_tx: mpsc::Sender<Deliverable>,
    result_rx: Option<mpsc::Receiver<Deliverable>>,

    // Feedback queue: Hermes pushes, workers consume
    feedback_tx: mpsc::Sender<Feedback>,
    feedback_rx: Option<mpsc::Receiver<Feedback>>,
}

/// Handle for cloning and sharing across agents
#[derive(Clone)]
pub struct BlackboardHandle {
    pub task_tx: mpsc::Sender<BlackboardTask>,
    pub event_tx: broadcast::Sender<SystemEvent>,
    pub result_tx: mpsc::Sender<Deliverable>,
    pub feedback_tx: mpsc::Sender<Feedback>,
}

impl Blackboard {
    pub fn new(capacity: usize) -> (Self, BlackboardHandle) {
        let (task_tx, task_rx) = mpsc::channel::<BlackboardTask>(capacity);
        let (result_tx, result_rx) = mpsc::channel::<Deliverable>(capacity);
        let (feedback_tx, feedback_rx) = mpsc::channel::<Feedback>(capacity);
        let (event_tx, _) = broadcast::channel::<SystemEvent>(256);

        let blackboard = Self {
            task_tx: task_tx.clone(),
            task_rx: Some(task_rx),
            event_tx: event_tx.clone(),
            result_tx: result_tx.clone(),
            result_rx: Some(result_rx),
            feedback_tx: feedback_tx.clone(),
            feedback_rx: Some(feedback_rx),
        };

        let handle = BlackboardHandle {
            task_tx,
            event_tx,
            result_tx,
            feedback_tx,
        };

        (blackboard, handle)
    }

    /// Sisyphus pushes tasks here
    pub async fn push_task(&self, task: BlackboardTask) -> anyhow::Result<()> {
        let task_id = task.id;
        self.task_tx.send(task).await?;
        let _ = self.event_tx.send(SystemEvent::TaskEnqueued { task_id });
        Ok(())
    }

    /// Prometheus/Atlas consume tasks
    pub async fn pop_task(&mut self) -> Option<BlackboardTask> {
        if let Some(rx) = &mut self.task_rx {
            rx.recv().await
        } else {
            None
        }
    }

    /// Workers push deliverables here
    pub async fn push_result(&self, result: Deliverable) -> anyhow::Result<()> {
        self.result_tx.send(result).await?;
        Ok(())
    }

    /// Hermes consumes results for validation
    pub async fn pop_result(&mut self) -> Option<Deliverable> {
        if let Some(rx) = &mut self.result_rx {
            rx.recv().await
        } else {
            None
        }
    }

    /// Hermes pushes feedback here
    pub async fn push_feedback(&self, feedback: Feedback) -> anyhow::Result<()> {
        self.feedback_tx.send(feedback).await?;
        Ok(())
    }

    /// Workers consume feedback for self-repair
    pub async fn pop_feedback(&mut self) -> Option<Feedback> {
        if let Some(rx) = &mut self.feedback_rx {
            rx.recv().await
        } else {
            None
        }
    }

    /// Subscribe to system events
    pub fn subscribe_events(&self) -> broadcast::Receiver<SystemEvent> {
        self.event_tx.subscribe()
    }

    /// Broadcast a system event
    pub fn broadcast_event(&self, event: SystemEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Take receivers (consumes the blackboard, returns receivers to caller)
    pub fn take_receivers(&mut self) -> (TaskReceiver, ResultReceiver, FeedbackReceiver) {
        (
            self.task_rx.take(),
            self.result_rx.take(),
            self.feedback_rx.take(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blackboard_task_push_pop() {
        let (mut bb, handle) = Blackboard::new(10);
        let task = BlackboardTask::new("test task", 0.5, TaskType::Code);
        let task_id = task.id;

        handle.task_tx.send(task).await.unwrap();
        let received = bb.pop_task().await.unwrap();
        assert_eq!(received.id, task_id);
    }

    #[tokio::test]
    async fn blackboard_result_push_pop() {
        let (mut bb, handle) = Blackboard::new(10);
        let task_id = Uuid::new_v4();
        let deliverable = Deliverable::new(task_id, "test-agent", "result content".to_string());

        handle.result_tx.send(deliverable).await.unwrap();
        let received = bb.pop_result().await.unwrap();
        assert_eq!(received.task_id, task_id);
        assert_eq!(received.content, "result content");
    }

    #[tokio::test]
    async fn blackboard_feedback_loop() {
        let (mut bb, handle) = Blackboard::new(10);
        let deliverable_id = Uuid::new_v4();
        let feedback = Feedback::new(deliverable_id, FeedbackErrorType::Semantic)
            .with_suggestions(vec!["Fix type error".to_string()]);

        handle.feedback_tx.send(feedback).await.unwrap();
        let received = bb.pop_feedback().await.unwrap();
        assert_eq!(received.deliverable_id, deliverable_id);
        assert_eq!(received.suggestions.len(), 1);
    }

    #[tokio::test]
    async fn blackboard_event_broadcast() {
        let (mut bb, _handle) = Blackboard::new(10);
        let mut subscriber = bb.subscribe_events();

        let task = BlackboardTask::new("test", 0.5, TaskType::Code);
        let expected_id = task.id;
        bb.push_task(task).await.unwrap();

        // Event should be received
        let event = tokio::time::timeout(std::time::Duration::from_millis(100), subscriber.recv())
            .await
            .unwrap()
            .unwrap();

        match event {
            SystemEvent::TaskEnqueued { task_id: id } => assert_eq!(id, expected_id),
            _ => panic!("Expected TaskEnqueued event"),
        }
    }

    #[test]
    fn feedback_increment_retry() {
        let mut feedback = Feedback::new(Uuid::new_v4(), FeedbackErrorType::Compilation);
        assert_eq!(feedback.retry_count, 0);
        feedback.increment_retry();
        assert_eq!(feedback.retry_count, 1);
    }

    #[test]
    fn deliverable_default_status() {
        let deliverable = Deliverable::new(Uuid::new_v4(), "agent", "content".to_string());
        assert_eq!(deliverable.validation_status, ValidationStatus::Pending);
        assert!(deliverable.arrow_data.is_none());
    }

    #[test]
    fn deliverable_arrow_data_attachment() {
        use arrow::array::StringArray;
        use arrow::datatypes::{DataType, Field, Schema};

        let schema = Arc::new(Schema::new(vec![Field::new("name", DataType::Utf8, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["Alice", "Bob"]))],
        )
        .unwrap();

        let deliverable = Deliverable::new(Uuid::new_v4(), "agent", "content".to_string())
            .with_arrow_data(Arc::new(batch));
        assert!(deliverable.arrow_data.is_some());
        assert_eq!(deliverable.arrow_data.unwrap().num_rows(), 2);
    }
}
