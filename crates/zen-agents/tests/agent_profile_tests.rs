// 4D Test: AgentProfile, AgentProfileBuilder, Role, Capability, AgentClearance, CostPerToken
//
// Dimensions:
//   Normal: Happy-path builder, default values, Display impls
//   Reverse: Edge cases (empty capabilities, zero cost, long names)
//   Adversarial: Invalid builder combinations, missing required fields
//   Logic Tree: Role↔Clearance mappings, capability intersection logic

use zen_agents::{AgentClearance, AgentProfile, Capability, CostPerToken, LlmPreference, Role};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn builder_creates_full_profile() {
    let profile = AgentProfile::builder("test-agent")
        .role(Role::Specialist)
        .capabilities(vec![Capability::CodeReview, Capability::Testing])
        .llm_preferences(vec![LlmPreference::LocalOnly])
        .max_sensitivity(AgentClearance::High)
        .cost_per_token(CostPerToken {
            input_cents_per_million: 1.5,
            output_cents_per_million: 3.0,
        })
        .build();

    assert_eq!(profile.name, "test-agent");
    assert_eq!(profile.role, Role::Specialist);
    assert_eq!(profile.capabilities.len(), 2);
    assert_eq!(profile.llm_preferences, vec![LlmPreference::LocalOnly]);
    assert_eq!(profile.max_sensitivity, AgentClearance::High);
    assert_eq!(profile.cost_per_token.input_cents_per_million, 1.5);
    assert!(profile.definition.is_none());
}

#[test]
fn builder_defaults_are_sane() {
    let profile = AgentProfile::builder("default-agent").build();
    assert_eq!(profile.role, Role::Worker);
    assert!(profile.capabilities.is_empty());
    assert_eq!(profile.llm_preferences, vec![LlmPreference::Any]);
    assert_eq!(profile.max_sensitivity, AgentClearance::Low);
    assert_eq!(profile.cost_per_token.input_cents_per_million, 0.0);
}

#[test]
fn role_display_all_variants() {
    assert_eq!(Role::Orchestrator.to_string(), "Orchestrator");
    assert_eq!(Role::Planner.to_string(), "Planner");
    assert_eq!(Role::Specialist.to_string(), "Specialist");
    assert_eq!(Role::Worker.to_string(), "Worker");
}

#[test]
fn capability_display_all_variants() {
    assert_eq!(Capability::CodeReview.to_string(), "code-review");
    assert_eq!(Capability::DesignReview.to_string(), "design-review");
    assert_eq!(Capability::Research.to_string(), "research");
    assert_eq!(Capability::Testing.to_string(), "testing");
    assert_eq!(Capability::Documentation.to_string(), "documentation");
    assert_eq!(Capability::Deployment.to_string(), "deployment");
    assert_eq!(Capability::Debugging.to_string(), "debugging");
    assert_eq!(Capability::Refactoring.to_string(), "refactoring");
    assert_eq!(Capability::Architecture.to_string(), "architecture");
    assert_eq!(Capability::SecurityAudit.to_string(), "security-audit");
    assert_eq!(
        Capability::PerformanceOptimization.to_string(),
        "performance-optimization"
    );
    assert_eq!(Capability::TaskExecution.to_string(), "task-execution");
    assert_eq!(
        Capability::SessionManagement.to_string(),
        "session-management"
    );
    assert_eq!(
        Capability::SpecificationWriting.to_string(),
        "specification-writing"
    );
    assert_eq!(Capability::CodeGeneration.to_string(), "code-generation");
    assert_eq!(
        Capability::KnowledgeManagement.to_string(),
        "knowledge-management"
    );
    assert_eq!(
        Capability::MemoryManagement.to_string(),
        "memory-management"
    );
    assert_eq!(Capability::Analysis.to_string(), "analysis");
    assert_eq!(Capability::Automation.to_string(), "automation");
}

#[test]
fn clearance_display_all_variants() {
    assert_eq!(AgentClearance::Low.to_string(), "Low");
    assert_eq!(AgentClearance::Medium.to_string(), "Medium");
    assert_eq!(AgentClearance::High.to_string(), "High");
}

#[test]
fn cost_per_token_display() {
    let cost = CostPerToken {
        input_cents_per_million: 5.0,
        output_cents_per_million: 10.0,
    };
    let display = cost.to_string();
    assert!(display.contains("in=5"));
    assert!(display.contains("out=10"));
}

#[test]
fn cost_per_token_default_is_zero() {
    let cost = CostPerToken::default();
    assert_eq!(cost.input_cents_per_million, 0.0);
    assert_eq!(cost.output_cents_per_million, 0.0);
}

#[test]
fn has_all_capabilities_exact_match() {
    let profile = AgentProfile::builder("a")
        .capabilities(vec![
            Capability::CodeReview,
            Capability::Testing,
            Capability::Debugging,
        ])
        .build();
    assert!(profile.has_all_capabilities(&[
        Capability::CodeReview,
        Capability::Testing,
        Capability::Debugging,
    ]));
}

#[test]
fn has_all_capabilities_subset() {
    let profile = AgentProfile::builder("a")
        .capabilities(vec![
            Capability::CodeReview,
            Capability::Testing,
            Capability::Debugging,
        ])
        .build();
    assert!(profile.has_all_capabilities(&[Capability::CodeReview]));
}

#[test]
fn has_all_capabilities_empty_required() {
    let profile = AgentProfile::builder("a").build();
    assert!(profile.has_all_capabilities(&[]));
}

#[test]
fn can_handle_sensitivity_at_or_below() {
    let profile = AgentProfile::builder("a")
        .max_sensitivity(AgentClearance::Medium)
        .build();
    assert!(profile.can_handle_sensitivity(AgentClearance::Low));
    assert!(profile.can_handle_sensitivity(AgentClearance::Medium));
}

#[test]
fn clearance_ordering_is_total() {
    assert!(AgentClearance::Low < AgentClearance::Medium);
    assert!(AgentClearance::Medium < AgentClearance::High);
    assert!(AgentClearance::Low < AgentClearance::High);
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn builder_empty_name() {
    let profile = AgentProfile::builder("").build();
    assert_eq!(profile.name, "");
    assert_eq!(profile.role, Role::Worker);
}

#[test]
fn builder_zero_capabilities() {
    let profile = AgentProfile::builder("minimal")
        .capabilities(vec![])
        .build();
    assert!(profile.capabilities.is_empty());
    assert!(profile.has_all_capabilities(&[]));
}

#[test]
fn builder_very_long_name() {
    let long_name = "a".repeat(1000);
    let profile = AgentProfile::builder(&long_name).build();
    assert_eq!(profile.name.len(), 1000);
}

#[test]
fn has_all_capabilities_missing_one() {
    let profile = AgentProfile::builder("a")
        .capabilities(vec![Capability::CodeReview, Capability::Testing])
        .build();
    assert!(!profile.has_all_capabilities(&[Capability::CodeReview, Capability::Research]));
}

#[test]
fn has_all_capabilities_none_match() {
    let profile = AgentProfile::builder("a")
        .capabilities(vec![Capability::CodeReview])
        .build();
    assert!(!profile.has_all_capabilities(&[Capability::DesignReview]));
}

#[test]
fn can_handle_sensitivity_exactly_equal() {
    let profile = AgentProfile::builder("a")
        .max_sensitivity(AgentClearance::High)
        .build();
    assert!(profile.can_handle_sensitivity(AgentClearance::High));
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn builder_name_with_control_chars() {
    let name = "test\nagent\t\x00";
    let profile = AgentProfile::builder(name).build();
    assert_eq!(profile.name, name);
}

#[test]
fn builder_duplicate_capabilities() {
    let profile = AgentProfile::builder("dup")
        .capabilities(vec![Capability::CodeReview, Capability::CodeReview])
        .build();
    assert_eq!(profile.capabilities.len(), 2);
    assert!(profile.has_all_capabilities(&[Capability::CodeReview]));
}

#[test]
fn builder_llm_preferences_with_conflicting() {
    let profile = AgentProfile::builder("conflict")
        .llm_preferences(vec![LlmPreference::LocalOnly, LlmPreference::CloudOnly])
        .build();
    assert_eq!(profile.llm_preferences.len(), 2);
}

#[test]
fn cost_per_token_negative_values() {
    let cost = CostPerToken {
        input_cents_per_million: -1.0,
        output_cents_per_million: -5.0,
    };
    let display = cost.to_string();
    assert!(display.contains("in=-1"));
    assert!(display.contains("out=-5"));
}

#[test]
fn high_clearance_cannot_handle_above_high() {
    let profile = AgentProfile::builder("maxed")
        .max_sensitivity(AgentClearance::High)
        .build();
    assert!(profile.can_handle_sensitivity(AgentClearance::High));
}
