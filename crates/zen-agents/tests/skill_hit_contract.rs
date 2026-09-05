//! T078 — contract tests for the skill-hit router (FR-037 / D03, contract
//! `skill-hit.json`): threshold 0.72, max_hits=1, disabled-skill never hits,
//! empty triggers never hit, and the orchestrator's pre-route M1 injection.

use std::fs;

use tempfile::tempdir;
use zen_agents::orchestrator::AgentOrchestrator;
use zen_agents::skill_hit_router::{SKILL_HIT_MAX_HITS, SKILL_HIT_THRESHOLD, SkillHitRouter};
use zen_agents::skill_loader::SkillDefinition;
use zen_core::types::{RetrievedNote, Sensitivity, SessionContext};
use zen_provider::DefaultRouter;

fn def(name: &str, triggers: &[&str]) -> SkillDefinition {
    SkillDefinition {
        name: name.to_string(),
        description: format!("{name} description"),
        triggers: triggers.iter().map(|t| t.to_string()).collect(),
        auto_route: None,
        tools: Vec::new(),
        context_files: Vec::new(),
        prompt: format!("procedure for {name}"),
        body: "body".to_string(),
    }
}

#[test]
fn contract_constants_match_skill_hit_json() {
    assert_eq!(SKILL_HIT_THRESHOLD, 0.72);
    assert_eq!(SKILL_HIT_MAX_HITS, 1);
}

#[test]
fn threshold_not_met_yields_no_hits() {
    let router = SkillHitRouter::new();
    let hits = router.route(
        "totally unrelated zebra qq qux",
        &[def("weekly-review", &["weekly review"])],
    );
    // contract error: "threshold_not_met"
    assert!(hits.is_empty());
}

#[test]
fn hit_at_threshold_reports_score_and_triggers() {
    let router = SkillHitRouter::new();
    let hits = router.route(
        "please run my weekly review",
        &[def(
            "weekly-review",
            &["weekly review", "non matching phrase"],
        )],
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].skill, "weekly-review");
    assert_eq!(hits[0].score, 1.0, "containment is a full-score hit");
    assert_eq!(hits[0].triggers_matched, vec!["weekly review"]);
}

#[test]
fn max_hits_is_one_best_score_wins() {
    let router = SkillHitRouter::new();
    let skills = vec![
        def("weak", &["fix the rust build error soon"]),
        def("exact", &["rust build"]),
    ];
    let hits = router.route("how do I fix my rust build", &skills);
    assert_eq!(
        hits.len(),
        1,
        "contract max_hits=1: only the best skill injects"
    );
    assert_eq!(hits[0].skill, "exact");
}

#[test]
fn disabled_skill_never_hits() {
    let mut opted_out = def("opted-out", &["weekly review"]);
    opted_out.auto_route = Some(false);
    assert!(!opted_out.is_auto_route_enabled());

    // The orchestrator filters before routing; a disabled skill must never
    // reach the router's candidate set.
    let eligible: Vec<SkillDefinition> = [opted_out]
        .into_iter()
        .filter(|d| d.is_auto_route_enabled())
        .collect();
    let router = SkillHitRouter::new();
    assert!(router.route("weekly review", &eligible).is_empty());
}

#[test]
fn empty_triggers_never_hit() {
    let router = SkillHitRouter::new();
    let hits = router.route("any query at all", &[def("mute", &[])]);
    assert!(hits.is_empty());
}

fn write_skill(home: &std::path::Path, name: &str, triggers: &str, auto_route: Option<bool>) {
    let skills = home.join("skills");
    fs::create_dir_all(&skills).expect("create skills dir");
    let opt_out = auto_route
        .map(|b| format!("\nauto_route: {b}"))
        .unwrap_or_default();
    fs::write(
        skills.join(format!("{name}.md")),
        format!(
            "---\nname: {name}\ndescription: review procedure\ntriggers: [{triggers}]{opt_out}\n---\n# {name}\nDo the review."
        ),
    )
    .expect("write skill file");
}

/// Env-dependent phases run in one test to avoid parallel env races.
#[test]
fn orchestrator_injects_skill_prompt_before_route_and_honors_gates() {
    // SAFETY: test-only env mutation; this integration binary's other tests
    // never read ZEN_HOME / ZEN_SKILLS_AUTO_ROUTE, and both vars are restored
    // at the end.
    let temp = tempdir().expect("tempdir");
    let home = temp.path().to_path_buf();
    let saved_home = std::env::var("ZEN_HOME").ok();
    let saved_route = std::env::var("ZEN_SKILLS_AUTO_ROUTE").ok();
    unsafe { std::env::set_var("ZEN_HOME", &home) };

    let router = DefaultRouter::new(zen_provider::LlmConfig::default());
    let orchestrator = AgentOrchestrator::new(router);

    // Phase 1: no skills on disk → no injection.
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    assert!(
        orchestrator
            .inject_skill_hits(&mut session, "weekly review")
            .is_none()
    );

    // Phase 2: matching skill → injected at the top of M1, capped top-5.
    write_skill(&home, "weekly-review", "weekly review", None);
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    for i in 0..7 {
        session.add_knowledge(vec![RetrievedNote {
            path: format!("wiki/page-{i}.md"),
            content: format!("page {i}"),
            sensitivity: Sensitivity::Public,
            relevance: 0.5,
        }]);
    }
    let hit = orchestrator
        .inject_skill_hits(&mut session, "please do my weekly review now")
        .expect("matching query must inject");
    assert_eq!(hit.skill, "weekly-review");
    assert_eq!(session.knowledge[0].path, "skills/weekly-review");
    assert!(
        session.knowledge[0].content.contains("Do the review"),
        "skill prompt must be the injected content"
    );
    assert_eq!(
        session.knowledge.len(),
        5,
        "M1 context capped at top-5 (Cowan 4)"
    );

    // Phase 3: non-matching query → no injection.
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    assert!(
        orchestrator
            .inject_skill_hits(&mut session, "what is the capital of France")
            .is_none()
    );

    // Phase 4: per-skill opt-out (frontmatter auto_route: false) never hits.
    write_skill(&home, "weekly-review", "weekly review", Some(false));
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    assert!(
        orchestrator
            .inject_skill_hits(&mut session, "weekly review")
            .is_none()
    );

    // Phase 5: global switch off → even a matching skill does not inject.
    write_skill(&home, "weekly-review", "weekly review", None);
    unsafe { std::env::set_var("ZEN_SKILLS_AUTO_ROUTE", "0") };
    // Integration binaries link the normally-compiled zen-core, where the
    // process-wide config cache is live (`#[cfg(test)]` does not propagate
    // to dependencies): invalidate so this env change is re-read.
    zen_core::config::invalidate_config_cache();
    let mut session = SessionContext::new("Sisyphus".into(), String::new());
    assert!(
        orchestrator
            .inject_skill_hits(&mut session, "weekly review")
            .is_none()
    );

    // Restore env.
    unsafe {
        match saved_route {
            Some(v) => std::env::set_var("ZEN_SKILLS_AUTO_ROUTE", v),
            None => std::env::remove_var("ZEN_SKILLS_AUTO_ROUTE"),
        }
        match saved_home {
            Some(v) => std::env::set_var("ZEN_HOME", v),
            None => std::env::remove_var("ZEN_HOME"),
        }
    }
}
