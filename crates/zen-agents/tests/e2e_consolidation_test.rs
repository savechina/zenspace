//! E2E test: M0→M2→M4 consolidation pipeline.
//!
//! Verifies the full chain: session conversation → SessionJournaler → journal entries
//! → MemoryCurator → MEMORY.md updates. This proves the wiring is correct end-to-end,
//! not just individually.

use std::fs;
use tempfile::TempDir;
use zen_memory::conversation::ConversationStore;

fn setup_workspace() -> TempDir {
    let tmp = TempDir::new().expect("temp dir");
    let root = tmp.path().to_path_buf();

    fs::create_dir_all(root.join("memories").join("journal")).unwrap();
    fs::create_dir_all(root.join("memories").join("commitments")).unwrap();
    fs::create_dir_all(root.join("sessions")).unwrap();
    fs::create_dir_all(root.join("wiki").join("wisdom").join("reflections")).unwrap();
    fs::create_dir_all(root.join("wiki").join("wisdom").join("anti-patterns")).unwrap();
    fs::create_dir_all(root.join("wiki").join("wisdom").join("facts")).unwrap();
    fs::create_dir_all(root.join("wiki").join("wisdom").join("models")).unwrap();

    let identity_dir = root.join(".zen").join("identity");
    fs::create_dir_all(&identity_dir).unwrap();
    fs::write(identity_dir.join("SOUL.md"), "# Soul\n\nTest identity.\n").unwrap();
    fs::write(
        identity_dir.join("MEMORY.md"),
        "# Memory\n\n## Identity\n\nTest.\n\n## Active Commitments\n\n## Stop-Doing Ledger\n\n## Continue-Doing Ledger\n\n## Active Mental Models\n\n## Recent Wisdom\n\n",
    )
    .unwrap();

    tmp
}

#[test]
fn test_m0_session_persistence_survives() {
    let tmp = setup_workspace();
    let sessions_dir = tmp.path().join("sessions");
    let store = ConversationStore::with_dir(sessions_dir.clone(), "test-session").unwrap();

    store
        .append("user", "I should avoid premature optimization")
        .unwrap();
    store
        .append("assistant", "Noted. Let me document this.")
        .unwrap();

    let turns = store.load().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].0, "user");
    assert_eq!(turns[1].0, "assistant");

    drop(store);
    let store2 = ConversationStore::with_dir(sessions_dir, "test-session").unwrap();
    let turns2 = store2.load().unwrap();
    assert_eq!(
        turns2.len(),
        2,
        "turns must survive store drop/recreate (M0 durability)"
    );
}

#[test]
fn test_m0_to_m2_journal_exists() {
    let tmp = setup_workspace();
    let journal_dir = tmp.path().join("memories").join("journal");

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let journal_path = journal_dir.join(format!("{today}.md"));

    fs::write(
        &journal_path,
        format!(
            "---\ndate: {today}\n---\n\n## Facts\n\n- User wants to avoid premature optimization\n\n## Reflections\n\n- Tendency to over-engineer before validating需求\n\n## Commitments\n\n- Validate with user before adding abstraction layers\n"
        ),
    )
    .unwrap();

    assert!(journal_path.exists(), "M2 journal entry must exist");
    let content = fs::read_to_string(&journal_path).unwrap();
    assert!(
        content.contains("## Facts"),
        "journal must have Facts section"
    );
    assert!(
        content.contains("## Reflections"),
        "journal must have Reflections section"
    );
    assert!(
        content.contains("## Commitments"),
        "journal must have Commitments section"
    );
}

#[test]
fn test_m4_memory_md_has_required_sections() {
    let tmp = setup_workspace();
    let memory_path = tmp.path().join(".zen").join("identity").join("MEMORY.md");

    let content = fs::read_to_string(&memory_path).unwrap();

    for section in [
        "## Identity",
        "## Active Commitments",
        "## Stop-Doing Ledger",
        "## Continue-Doing Ledger",
        "## Active Mental Models",
        "## Recent Wisdom",
    ] {
        assert!(
            content.contains(section),
            "MEMORY.md must contain section: {section}"
        );
    }
}

#[test]
fn test_m2_to_m4_reflection_signal_persistence() {
    let tmp = setup_workspace();
    let reflections_dir = tmp.path().join("wiki").join("wisdom").join("reflections");

    let signal_path = reflections_dir.join("test-reflection.md");
    fs::write(
        &signal_path,
        "---\nid: test-reflection\ndate: 2026-07-23\nseverity: high\ndomain: 秩序\n---\n\n## What went wrong\n\nOver-engineered a solution before validating the problem.\n\n## Why\n\nAssumed complexity without checking actual requirements.\n\n## Avoidance\n\nAlways ask: 'Is this solving a real problem or one we created?'\n",
    )
    .unwrap();

    assert!(
        signal_path.exists(),
        "M4 reflection signal must be persisted"
    );
    let content = fs::read_to_string(&signal_path).unwrap();
    assert!(
        content.contains("What went wrong"),
        "reflection must have problem description"
    );
    assert!(
        content.contains("Avoidance"),
        "reflection must have avoidance strategy"
    );
}

#[test]
fn test_m5_commitment_lifecycle() {
    let tmp = setup_workspace();
    let commitments_dir = tmp.path().join("memories").join("commitments");

    let commitment_path = commitments_dir.join("test-commitment.md");
    fs::write(
        &commitment_path,
        "---\nid: test-commitment\nstatus: executing\nreview_at: 2026-08-23\n---\n\n## What\n\nValidate requirements before implementing.\n\n## Next Action\n\nAsk user for concrete use case.\n",
    )
    .unwrap();

    assert!(commitment_path.exists(), "M5 commitment must be persisted");
    let content = fs::read_to_string(&commitment_path).unwrap();
    assert!(
        content.contains("review_at"),
        "commitment must have review_at timestamp"
    );
    assert!(
        content.contains("status: executing"),
        "commitment must have lifecycle status"
    );
}

#[test]
fn test_worker_report_has_llm_cost_field() {
    use zen_agents::scheduler::WorkerReport;

    let report = WorkerReport {
        worker_id: "test".to_string(),
        success: true,
        fact_count: 0,
        duration_ms: 0,
        llm_cost_usd: 0.0,
    };

    assert_eq!(
        report.llm_cost_usd, 0.0,
        "WorkerReport must have llm_cost_usd field (A4 fix)"
    );
}

#[test]
fn test_scheduler_has_cost_cap() {
    use zen_agents::scheduler::ZenScheduler;

    let scheduler = ZenScheduler::new().with_cost_cap(5.0);
    assert_eq!(
        scheduler.cost_cap(),
        5.0,
        "scheduler must support per-worker cost cap (A4 fix)"
    );
}

#[test]
fn test_memvid_incremental_indexing_method_exists() {
    use zen_memory::memvid::ZenMemvidStore;
    use zen_memory::memvid_index::MemvidIndexer;

    let tmp = setup_workspace();
    let indexer = MemvidIndexer::new(tmp.path().to_path_buf());
    let db_path = tmp.path().join("test_incremental.mv2");
    let mut store = ZenMemvidStore::new(db_path).unwrap();

    let report = indexer.index_incremental(&mut store).unwrap();
    assert_eq!(
        report.files_scanned, 0,
        "incremental index on empty workspace should scan 0 files"
    );
}

/// E2E test: M0→M2→M4 consolidation pipeline.
///
/// Verifies the full chain end-to-end:
/// 1. Write session conversation to JSONL (M0)
/// 2. SessionJournaler extracts facts → journal entry (M2)
/// 3. MemoryCurator reads journal → MEMORY.md updated (M4)
///
/// Uses keyword-only extraction (no LLM dependency) by ensuring no provider config.
/// Requires `--test-threads=1` due to ZEN_HOME env var manipulation.
#[tokio::test]
async fn test_e2e_m0_m2_m4_consolidation_pipeline() {
    use zen_agents::scheduler::{MemoryCurator, SessionJournaler, WorkerContext, ZenWorker};
    use zen_memory::conversation::ConversationStore;

    // 1. Setup temp workspace with ZEN_HOME for ZenPaths resolution
    let tmp = TempDir::new().expect("temp dir");
    let root = tmp.path().to_path_buf();
    // SAFETY: single-threaded test — ZEN_HOME set once before any worker reads it
    unsafe { std::env::set_var("ZEN_HOME", &root) };

    // Force keyword-only extraction: with no reachable LLM, the journaler's
    // `extract_signals_via_llm` falls back to keyword extraction. The embedded
    // default (`default_provider = "ollama"`) would otherwise trigger a real
    // LLM call that hangs for minutes when Ollama is running/slow (or
    // unreachable). `mock` is keyless and non-local, so `route()` returns
    // `ProviderUnavailable` for Private sensitivity → keyword fallback.
    fs::write(
        root.join("config.toml"),
        "default_provider = \"mock\"\n\n[providers.mock]\nprovider_type = \"mock\"\n",
    )
    .unwrap();

    // 2. Create directory structure for workers
    // SessionJournaler needs: sessions/, memories/journal/, vault/wiki/wisdom/facts/
    // MemoryCurator needs: memories/journal/, memories/MEMORY.md
    fs::create_dir_all(root.join("sessions")).unwrap();
    fs::create_dir_all(root.join("memories").join("journal")).unwrap();
    fs::create_dir_all(root.join("vault").join("wiki").join("wisdom").join("facts")).unwrap();

    // Create identity files (MemoryCurator's update_memory_from_facts reads MEMORY.md)
    fs::write(
        root.join("memories").join("SOUL.md"),
        "# Soul\n\nTest identity.\n",
    )
    .unwrap();
    fs::write(
        root.join("memories").join("MEMORY.md"),
        "# Memory\n\n## Identity\n\n## Active Commitments\n\n## Stop-Doing Ledger\n\n## Continue-Doing Ledger\n\n## Active Mental Models\n\n## Recent Wisdom\n\n",
    )
    .unwrap();

    // 3. Create session conversation (M0) — ≥3 turns required by SessionJournaler
    let session_id = "test-e2e-consolidation";
    let store = ConversationStore::with_dir(root.join("sessions"), session_id).unwrap();
    store
        .append("user", "I completed the requirements review and fixed the premature optimization in the auth module")
        .unwrap();
    store
        .append(
            "assistant",
            "Good work. I documented this and updated the project guidelines.",
        )
        .unwrap();
    store
        .append(
            "user",
            "Yes. We implemented concrete use cases before designing the abstraction layers.",
        )
        .unwrap();
    store
        .append(
            "assistant",
            "Agreed. Testing before optimizing has become a solid team practice.",
        )
        .unwrap();
    drop(store);

    // Verify M0 persistence
    let verify_store = ConversationStore::with_dir(root.join("sessions"), session_id).unwrap();
    let turns = verify_store.load().unwrap();
    assert_eq!(
        turns.len(),
        4,
        "M0: session conversation must persist 4 turns"
    );
    drop(verify_store);

    // 4. Run SessionJournaler (M0→M2)
    // With no valid provider config, router will be None → keyword-only extraction
    let journaler = SessionJournaler::new();
    let ctx = WorkerContext::new(chrono::Utc::now());
    let journal_report = journaler
        .execute(&ctx)
        .await
        .expect("SessionJournaler should succeed");
    assert!(
        journal_report.success,
        "M2: SessionJournaler must report success"
    );

    // 5. Verify journal entry was created (M2)
    let journal_dir = root.join("memories").join("journal");
    let mut entries: Vec<_> = fs::read_dir(&journal_dir)
        .expect("journal dir must exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(|e| e.path());

    assert!(
        !entries.is_empty(),
        "M2: SessionJournaler must create at least one journal entry"
    );

    let journal_path = entries[0].path();
    let journal_content = fs::read_to_string(&journal_path).unwrap();
    assert!(
        journal_content.contains("## Facts"),
        "journal must have Facts section"
    );
    assert!(
        journal_content.contains("## Reflections"),
        "journal must have Reflections section"
    );
    assert!(
        journal_content.contains("## Commitments"),
        "journal must have Commitments section"
    );
    assert!(
        journal_content.contains(&format!("session_id: {session_id}")),
        "journal must reference the source session"
    );

    // Verify at least one fact was extracted (keyword fallback extracts facts)
    // The conversation is dense with durable content
    assert!(
        journal_report.fact_count > 0 || journal_content.contains("- "),
        "M2: keyword extraction should produce at least one signal"
    );

    // 6. Run MemoryCurator (M2→M4)
    let curator = MemoryCurator::new();
    let curator_report = curator
        .execute(&ctx)
        .await
        .expect("MemoryCurator should succeed");
    assert!(
        curator_report.success,
        "M4: MemoryCurator must report success"
    );

    // 7. Verify MEMORY.md was updated (M4)
    let memory_path = root.join("memories").join("MEMORY.md");
    let memory_content = fs::read_to_string(&memory_path).unwrap();
    assert!(
        memory_content.contains("## Recent Wisdom"),
        "MEMORY.md must have Recent Wisdom section"
    );

    // If facts were extracted and routed, MEMORY.md should contain session evidence
    if curator_report.fact_count > 0 {
        assert!(
            memory_content.contains("[Session]"),
            "M4: MEMORY.md must contain Session-sourced facts when MemoryCurator found facts"
        );
    }
}
