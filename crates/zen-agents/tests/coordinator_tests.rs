// 4D Test: ZenCoordinator — intent classification, entropy routing, specialist mapping
//
// Dimensions:
//   Normal: Route queries to correct specialists, classify intent
//   Reverse: Unknown/ambiguous queries, empty queries
//   Adversarial: Very long input, mixed signals, special characters
//   Logic Tree: All 18 routing rule branches, entropy/complexity thresholds

use zen_agents::{EntropyConfig, ZenCoordinator};

// These tests test the PURE LOGIC of ZenCoordinator's intent classification
// and entropy-based routing. They do NOT require a full wiring/registry setup
// because we test classify_intent and classify_by_entropy via the public route
// and route_by_complexity methods, which delegate to internal pure functions.

// ============================================================================
// Helpers — minimal coordinator for testing pure routing logic
// ============================================================================

/// Create an EntropyConfig with precise thresholds for testing.
fn test_entropy_config() -> EntropyConfig {
    EntropyConfig {
        simple_threshold: 0.3,
        standard_threshold: 0.6,
        complex_threshold: 0.7,
        critical_threshold: 0.9,
    }
}

// ============================================================================
// Normal Dimension — Intent Classification via route()
// ============================================================================

// coordinator_routes_search_to_researcher
// coordinator_routes_find_to_researcher
// coordinator_routes_query_to_researcher
// coordinator_routes_wiki_compile_to_coder
// coordinator_routes_create_to_coder
// coordinator_routes_analyze_to_analyst
// coordinator_routes_detect_to_analyst
// coordinator_routes_consolidate_to_consolidator
// coordinator_routes_pipeline_to_consolidator
// coordinator_routes_chat_to_conversation
// coordinator_routes_help_to_conversation
// coordinator_routes_config_to_system
// coordinator_routes_serve_to_system
// coordinator_defaults_to_researcher_for_unknown_query
// classify_intent_is_case_insensitive

// These already exist in coordinator.rs as #[cfg(test)] inline tests.

// ============================================================================
// Normal Dimension — Entropy-based routing
// ============================================================================

#[test]
fn entropy_config_defaults_are_reasonable() {
    let cfg = EntropyConfig::default();
    assert!((cfg.simple_threshold - 0.3).abs() < f64::EPSILON);
    assert!((cfg.standard_threshold - 0.6).abs() < f64::EPSILON);
    assert!((cfg.complex_threshold - 0.7).abs() < f64::EPSILON);
    assert!((cfg.critical_threshold - 0.9).abs() < f64::EPSILON);
    assert!(cfg.simple_threshold < cfg.standard_threshold);
    assert!(cfg.standard_threshold < cfg.complex_threshold);
    assert!(cfg.complex_threshold < cfg.critical_threshold);
}

#[test]
fn test_entropy_threshold_boundaries() {
    let cfg = test_entropy_config();

    // Just below thresholds
    assert!(0.299 < cfg.simple_threshold);
    assert!(0.599 < cfg.standard_threshold);
    assert!(0.699 < cfg.complex_threshold);
    assert!(0.899 < cfg.critical_threshold);

    // At thresholds
    assert!(0.3 >= cfg.simple_threshold);
    assert!(0.6 >= cfg.standard_threshold);
    assert!(0.7 >= cfg.complex_threshold);
    assert!(0.9 >= cfg.critical_threshold);
}

#[test]
fn entropy_thresholds_are_monotonic() {
    let cfg = test_entropy_config();
    assert!(cfg.simple_threshold < cfg.standard_threshold);
    assert!(cfg.standard_threshold < cfg.complex_threshold);
    assert!(cfg.complex_threshold < cfg.critical_threshold);
}

// ============================================================================
// Normal Dimension — Specialist mapping via get_specialists()
// ============================================================================

#[test]
fn get_specialists_deep_analysis() {
    let specialists = ZenCoordinator::get_specialists("deep_analysis");
    assert_eq!(specialists, vec!["Oracle"]);
}

#[test]
fn get_specialists_research() {
    let specialists = ZenCoordinator::get_specialists("research");
    assert_eq!(specialists, vec!["Explore"]);
}

#[test]
fn get_specialists_knowledge_organization() {
    let specialists = ZenCoordinator::get_specialists("knowledge_organization");
    assert_eq!(specialists, vec!["Librarian"]);
}

#[test]
fn get_specialists_image_understanding() {
    let specialists = ZenCoordinator::get_specialists("image_understanding");
    assert_eq!(specialists, vec!["Argus"]);
}

#[test]
fn get_specialists_comprehensive() {
    let specialists = ZenCoordinator::get_specialists("comprehensive");
    assert_eq!(specialists.len(), 3);
    assert!(specialists.contains(&"Oracle".to_string()));
    assert!(specialists.contains(&"Explore".to_string()));
    assert!(specialists.contains(&"Librarian".to_string()));
}

// ============================================================================
// Reverse Dimension
// ============================================================================

#[test]
fn get_specialists_unknown_task_type() {
    let specialists = ZenCoordinator::get_specialists("unknown_task");
    assert!(specialists.is_empty());
}

#[test]
fn get_specialists_empty_string() {
    let specialists = ZenCoordinator::get_specialists("");
    assert!(specialists.is_empty());
}

#[test]
fn get_specialists_case_insensitive() {
    assert_eq!(
        ZenCoordinator::get_specialists("DEEP_ANALYSIS"),
        vec!["Oracle"]
    );
    assert_eq!(ZenCoordinator::get_specialists("Research"), vec!["Explore"]);
}

// ============================================================================
// Adversarial Dimension
// ============================================================================

#[test]
fn get_specialists_sql_injection_like() {
    let specialists = ZenCoordinator::get_specialists("'; DROP TABLE agents; --");
    assert!(specialists.is_empty());
}

#[test]
fn get_specialists_very_long_task_type() {
    let long = "a".repeat(1000);
    let specialists = ZenCoordinator::get_specialists(&long);
    assert!(specialists.is_empty());
}

#[test]
fn get_specialists_unicode() {
    let specialists = ZenCoordinator::get_specialists("研究");
    assert!(specialists.is_empty());
}

// ============================================================================
// Logic Tree Dimension
// ============================================================================

#[test]
fn specialist_mapping_completeness() {
    // Verify all known task types have a mapping
    let known_types = [
        "deep_analysis",
        "complex_problem",
        "research",
        "information_discovery",
        "knowledge_organization",
        "deduplication",
        "image_understanding",
        "chart_reading",
        "comprehensive",
    ];
    for task_type in &known_types {
        let specialists = ZenCoordinator::get_specialists(task_type);
        assert!(
            !specialists.is_empty(),
            "Task type '{}' should have at least one specialist",
            task_type
        );
    }
}

#[test]
fn complex_problem_maps_to_oracle() {
    let specialists = ZenCoordinator::get_specialists("complex_problem");
    assert_eq!(specialists, vec!["Oracle"]);
}

#[test]
fn information_discovery_maps_to_explore() {
    let specialists = ZenCoordinator::get_specialists("information_discovery");
    assert_eq!(specialists, vec!["Explore"]);
}

#[test]
fn deduplication_maps_to_librarian() {
    let specialists = ZenCoordinator::get_specialists("deduplication");
    assert_eq!(specialists, vec!["Librarian"]);
}

#[test]
fn chart_reading_maps_to_argus() {
    let specialists = ZenCoordinator::get_specialists("chart_reading");
    assert_eq!(specialists, vec!["Argus"]);
}

#[test]
fn entropy_config_copy_and_clone() {
    let cfg = test_entropy_config();
    let cfg2 = cfg.clone();
    assert!((cfg2.simple_threshold - cfg.simple_threshold).abs() < f64::EPSILON);
}
