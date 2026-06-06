// 4D Test: MomusReviewer plan quality gate, QualityPipeline
//
// Dimensions:
//   Normal: Simple plan approval, detailed plan approval
//   Reverse: Empty/malformed plans, excessive steps
//   Adversarial: Conflicting plan actions, ambiguous references
//   Logic Tree: All veto triggers (inconsistency, untestable, unverifiable)

use zen_agents::review::MomusReviewer;
use zen_core::types::{Task, TaskType};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn momus_approves_simple_plan() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("test task", 0.3, TaskType::Code);
    let review = reviewer.gate_review(&task, "Implement feature X");
    assert!(review.approved);
    assert!(review.veto_reason.is_none());
}

#[test]
fn momus_approves_plan_with_verification() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("add auth", 0.4, TaskType::Code);
    let plan = "Add JWT middleware. Write tests to verify. Check coverage.";
    let review = reviewer.gate_review(&task, plan);
    assert!(review.approved);
}

#[test]
fn momus_approves_plan_with_measurable_criteria() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("optimize", 0.5, TaskType::Code);
    let plan = "Reduce page load time below 2 seconds. Verify with Lighthouse score > 90.";
    let review = reviewer.gate_review(&task, plan);
    assert!(review.approved);
}

#[test]
fn check_plan_consistency_no_issues() {
    let reviewer = MomusReviewer::new();
    let findings = reviewer.check_plan_consistency("Add new feature. Write tests. Document.");
    assert!(findings.is_empty());
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn momus_approves_empty_plan() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("empty", 0.1, TaskType::Text);
    let review = reviewer.gate_review(&task, "");
    assert!(review.approved);
}

#[test]
fn momus_approves_whitespace_plan() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("ws", 0.1, TaskType::Text);
    let review = reviewer.gate_review(&task, "   \n  \t  ");
    assert!(review.approved);
}

#[test]
fn check_plan_consistency_empty_returns_empty() {
    let reviewer = MomusReviewer::new();
    let findings = reviewer.check_plan_consistency("");
    assert!(findings.is_empty());
}

#[test]
fn validate_acceptance_criteria_empty_returns_empty() {
    let reviewer = MomusReviewer::new();
    let findings = reviewer.validate_acceptance_criteria("");
    assert!(findings.is_empty());
}

#[test]
fn assess_verifiability_empty_returns_empty() {
    let reviewer = MomusReviewer::new();
    let findings = reviewer.assess_verifiability("");
    assert!(findings.is_empty());
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn momus_veto_on_create_and_delete() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("create then delete", 0.7, TaskType::Code);
    let plan = "create the new table. delete the old table. verify it works";
    let review = reviewer.gate_review(&task, plan);
    assert!(!review.approved, "Plan with create+delete should be vetoed");
    assert!(review.veto_reason.is_some());
    assert!(review.veto_reason.unwrap().contains("PlanInconsistency"));
}

#[test]
fn momus_veto_on_no_verification() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("big change", 0.6, TaskType::Code);
    let plan =
        "Refactor the entire module to use a new pattern. Migrate all callers. Update configs.";
    let review = reviewer.gate_review(&task, plan);
    // Long plan without verification keywords → blocking finding
    assert!(!review.approved);
}

#[test]
fn momus_detects_circular_step_references() {
    let reviewer = MomusReviewer::new();
    let plan = "step 1: do thing\nstep 2: step 3\nstep 3: step 1\nstep 4: step 2\nstep 5: step 4\nstep 6: step 5\nstep 7: step 6";
    let findings = reviewer.check_plan_consistency(plan);
    assert!(
        findings
            .iter()
            .any(|f| f.finding_type.to_string().contains("Inconsistency")),
        "Should detect circular step references"
    );
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn can_veto_true_when_blocking_findings_exist() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("test", 0.5, TaskType::Code);
    let review = reviewer.gate_review(&task, "create new. delete old.");
    assert!(reviewer.can_veto(&review));
}

#[test]
fn can_veto_false_when_no_blocking_findings() {
    let reviewer = MomusReviewer::new();
    let task = Task::new("test", 0.5, TaskType::Text);
    let review = reviewer.gate_review(&task, "test plan");
    assert!(!reviewer.can_veto(&review));
}

#[test]
fn task_semantic_entropy_preserved() {
    let task = Task::new("high entropy task", 0.95, TaskType::Code);
    assert!((task.semantic_entropy - 0.95).abs() < f64::EPSILON);
    assert_eq!(task.user_input, "high entropy task");
}

#[test]
fn validate_acceptance_criteria_detects_no_criteria() {
    let reviewer = MomusReviewer::new();
    let plan = "Just do the thing. Make it work. Ship it.";
    let findings = reviewer.validate_acceptance_criteria(plan);
    assert!(
        findings.iter().any(|f| matches!(
            f.finding_type,
            zen_agents::review::MomusFindingType::UntestableAcceptanceCriteria
        )),
        "Should flag missing acceptance criteria"
    );
}
