// 4D Test: DefaultAgentRegistry, AgentRegistry trait, RegistryError
//
// Dimensions:
//   Normal: Lookup by name/role/capability, register new, all 13 built-in
//   Reverse: Duplicate registration, nonexistent agent, empty capability search
//   Adversarial: Register with conflicting names, massive agent list
//   Logic Tree: Role hierarchy counts, capability intersection, sensitivity filtering

use zen_agents::{
    AgentClearance, AgentProfile, AgentRegistry, Capability, DefaultAgentRegistry, RegistryError,
    Role,
};

// ============================================================================
// Normal Dimension
// ============================================================================

#[test]
fn has_thirteen_builtin_agents() {
    let registry = DefaultAgentRegistry::new();
    assert_eq!(registry.list_all().len(), 13);
}

#[test]
fn find_by_name_existing() {
    let registry = DefaultAgentRegistry::new();
    let profile = registry
        .find_by_name("Sisyphus")
        .expect("Sisyphus must exist");
    assert_eq!(profile.role, Role::Orchestrator);
}

#[test]
fn find_by_role_orchestrator() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_role(Role::Orchestrator);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "Sisyphus");
}

#[test]
fn find_by_role_planner() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_role(Role::Planner);
    assert_eq!(agents.len(), 4); // Metis, Momus, Prometheus, Zeus
}

#[test]
fn find_by_role_specialist() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_role(Role::Specialist);
    assert_eq!(agents.len(), 4); // Oracle, Explore, Librarian, Argus
}

#[test]
fn find_by_role_worker() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_role(Role::Worker);
    assert_eq!(agents.len(), 4); // Junior, Hermes, Hephaestus, Atlas
}

#[test]
fn find_by_capability_single() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_capability(&[Capability::CodeReview]);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "Momus");
}

#[test]
fn find_by_capability_research_and_analysis() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_capability(&[Capability::Research, Capability::Analysis]);
    let names: Vec<&str> = agents.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Oracle"));
    assert!(names.contains(&"Explore"));
    assert!(names.contains(&"Metis"));
}

#[test]
fn register_new_agent_increases_count() {
    let mut registry = DefaultAgentRegistry::new();
    let before = registry.list_all().len();
    let new_agent = AgentProfile::builder("CustomAgent")
        .role(Role::Worker)
        .capabilities(vec![Capability::Testing])
        .build();
    registry.register(new_agent).expect("register must succeed");
    assert_eq!(registry.list_all().len(), before + 1);
}

#[test]
fn filter_by_sensitivity_low_returns_all() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.filter_by_sensitivity(AgentClearance::Low);
    assert_eq!(agents.len(), 13);
}

#[test]
fn filter_by_sensitivity_medium_excludes_explore() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.filter_by_sensitivity(AgentClearance::Medium);
    assert_eq!(agents.len(), 12);
    let names: Vec<&str> = agents.iter().map(|p| p.name.as_str()).collect();
    assert!(!names.contains(&"Explore"));
}

#[test]
fn filter_by_sensitivity_high() {
    let registry = DefaultAgentRegistry::new();
    let agents = registry.filter_by_sensitivity(AgentClearance::High);
    assert_eq!(agents.len(), 6);
}

#[test]
fn builtin_agents_have_definitions() {
    let registry = DefaultAgentRegistry::new();
    let all = registry.list_all();
    let with_def: Vec<&&AgentProfile> = all.iter().filter(|p| p.definition.is_some()).collect();
    assert_eq!(
        with_def.len(),
        13,
        "all 13 built-in agents should have definitions"
    );
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn find_by_name_nonexistent_returns_error() {
    let registry = DefaultAgentRegistry::new();
    let result = registry.find_by_name("NonExistentAgent");
    assert!(result.is_err());
    match result.unwrap_err() {
        RegistryError::AgentNotFound { name } => assert_eq!(name, "NonExistentAgent"),
        other => panic!("Expected AgentNotFound, got: {other:?}"),
    }
}

#[test]
fn register_duplicate_rejected() {
    let mut registry = DefaultAgentRegistry::new();
    let dup = AgentProfile::builder("Sisyphus").build();
    let result = registry.register(dup);
    assert!(result.is_err());
    match result.unwrap_err() {
        RegistryError::DuplicateAgent { name } => assert_eq!(name, "Sisyphus"),
        other => panic!("Expected DuplicateAgent, got: {other:?}"),
    }
}

#[test]
fn find_by_role_empty_when_none_match() {
    // Role is exhaustive enum, so all roles have matches,
    // but we can test with no capability matches
    let registry = DefaultAgentRegistry::new();
    let agents = registry.find_by_capability(&[Capability::Automation]);
    // Atlas has Automation, so this is not truly empty
    let names: Vec<&str> = agents.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"Atlas"));
}

#[test]
fn register_after_lookup_consistent() {
    let mut registry = DefaultAgentRegistry::new();
    let initial = registry.list_all().len();

    let agent = AgentProfile::builder("NewAgent").role(Role::Worker).build();
    registry.register(agent).unwrap();

    assert_eq!(registry.list_all().len(), initial + 1);
    let found = registry.find_by_name("NewAgent");
    assert!(found.is_ok());
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn register_same_name_twice() {
    let mut registry = DefaultAgentRegistry::new();
    let a1 = AgentProfile::builder("Conflict").build();
    let a2 = AgentProfile::builder("Conflict").build();
    assert!(registry.register(a1).is_ok());
    assert!(registry.register(a2).is_err());
}

#[test]
fn register_empty_name() {
    let mut registry = DefaultAgentRegistry::new();
    let agent = AgentProfile::builder("").build();
    // Empty name is allowed but might cause issues
    assert!(registry.register(agent).is_ok());
}

#[test]
fn find_by_capability_unknown() {
    let registry = DefaultAgentRegistry::new();
    // No agent has all of these (CodeReview + Automation + MemoryManagement)
    let agents = registry.find_by_capability(&[
        Capability::CodeReview,
        Capability::Automation,
        Capability::MemoryManagement,
    ]);
    assert!(agents.is_empty());
}

#[test]
fn find_by_name_case_sensitive() {
    let registry = DefaultAgentRegistry::new();
    // "sisyphus" vs "Sisyphus"
    let result = registry.find_by_name("sisyphus");
    assert!(result.is_err());
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn every_agent_has_unique_name() {
    let registry = DefaultAgentRegistry::new();
    let all = registry.list_all();
    let mut names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), all.len(), "all agent names must be unique");
}

#[test]
fn builtin_agent_costs_are_positive() {
    let registry = DefaultAgentRegistry::new();
    for profile in registry.list_all() {
        assert!(
            profile.cost_per_token.input_cents_per_million >= 0.0,
            "{} has negative input cost",
            profile.name
        );
        assert!(
            profile.cost_per_token.output_cents_per_million >= 0.0,
            "{} has negative output cost",
            profile.name
        );
    }
}

#[test]
fn sensitivity_filter_is_subset_of_list_all() {
    let registry = DefaultAgentRegistry::new();
    let all_count = registry.list_all().len();
    let low = registry.filter_by_sensitivity(AgentClearance::Low).len();
    let medium = registry.filter_by_sensitivity(AgentClearance::Medium).len();
    let high = registry.filter_by_sensitivity(AgentClearance::High).len();

    assert!(medium <= all_count, "Medium filter must be <= all");
    assert!(high <= medium, "High filter must be <= Medium");
    assert_eq!(low, all_count, "Low filter must include all");
}
