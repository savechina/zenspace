use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouterExt};
use zen_vault::entity::{Entity, EntityService, EntityType};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

const TECH_KEYWORDS: &[&str] = &[
    "Rust", "Python", "TypeScript", "JavaScript",
    "CLIP", "LLM", "GPT", "OpenAI",
    "Anthropic", "DeepSeek", "Gemini",
    "Tokio", "async", "sqlite", "SQLite",
    "FTS5", "vector", "embedding",
];

const MIN_CONTENT_LEN: usize = 20;

pub struct EntityExtractorWorker {
    scheduled: Option<&'static str>,
}

impl EntityExtractorWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for EntityExtractorWorker {
    fn id(&self) -> &'static str {
        "entity-extractor"
    }

    fn description(&self) -> &'static str {
        "Scan journal entries, extract entities via LLM with keyword fallback, upsert to graph.db"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */10 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let journal_dir = paths.journal_entries();
        if !journal_dir.is_dir() {
            debug!("journal entries directory does not exist, skipping");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let entries = scan_journal_entries(&journal_dir)?;
        if entries.is_empty() {
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let graph_db = paths.db().join("graph.db");
        let svc = EntityService::new();

        let mut known = svc.load_known_entity_names(&graph_db).unwrap_or_default();
        for kw in TECH_KEYWORDS {
            known.insert((*kw).to_string());
        }

        let router: Option<DefaultRouter> = match load_config() {
            Ok(c) => Some(DefaultRouter::from_agentic(c)),
            Err(e) => {
                warn!(error = %e, "failed to load config for LLM entity extraction, falling back to keyword-only");
                None
            }
        };

        let mut processed = 0usize;
        let mut total_entities = 0usize;

        for entry_path in &entries {
            if has_extracted_marker(entry_path) {
                continue;
            }

            match process_entry(entry_path, &svc, &graph_db, &known, router.clone()).await {
                Ok(count) => {
                    total_entities += count;
                    processed += 1;
                    if count > 0 {
                        info!(path = %entry_path.display(), entities = count, "entities extracted from journal entry");
                    }
                }
                Err(e) => {
                    warn!(path = %entry_path.display(), error = %e, "failed to extract entities from journal entry");
                }
            }
        }

        if processed > 0 {
            info!(processed, entities = total_entities, "entity-extractor tick complete");
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_entities,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

async fn process_entry(
    entry_path: &std::path::Path,
    svc: &EntityService,
    graph_db: &std::path::Path,
    known: &HashSet<String>,
    router: Option<DefaultRouter>,
) -> Result<usize> {
    let content = fs::read_to_string(entry_path)
        .with_context(|| format!("failed to read journal entry: {}", entry_path.display()))?;

    if content.len() < MIN_CONTENT_LEN {
        return Ok(0);
    }

    let facts = extract_facts_from_journal(&content);
    if facts.is_empty() {
        append_extracted_marker(entry_path, "keyword")?;
        return Ok(0);
    }

    let matched = if let Some(router) = router {
        let entry_path_clone = entry_path.to_path_buf();
        let content_clone = content.clone();
        let facts_clone = facts.clone();
        let llm_result = tokio::task::spawn_blocking(move || {
            extract_entities_via_llm(&entry_path_clone, &content_clone, &facts_clone, router)
        })
        .await
        .context("LLM extraction task panicked")?;

        match llm_result {
            Ok(llm_entities) if !llm_entities.is_empty() => {
                info!(path = %entry_path.display(), count = llm_entities.len(), "LLM entity extraction succeeded");
                let count = upsert_entities(llm_entities, svc, graph_db)?;
                append_extracted_marker(entry_path, "llm")?;
                return Ok(count);
            }
            Ok(_) => {
                debug!(path = %entry_path.display(), "LLM returned no entities, falling back to keyword");
            }
            Err(e) => {
                warn!(path = %entry_path.display(), error = %e, "LLM extraction failed, falling back to keyword");
            }
        }
        match_entities(&facts, known)
    } else {
        match_entities(&facts, known)
    };

    if matched.is_empty() {
        append_extracted_marker(entry_path, "keyword")?;
        return Ok(0);
    }

    let mut upserted = 0usize;
    for (entity_name, fact_list) in &matched {
        let canonical = entity_name.to_lowercase();
        let entity = Entity::new(canonical, EntityType::Technology, "entity-extractor");
        if let Err(e) = svc.upsert_entity(graph_db, &entity) {
            warn!(entity = %entity_name, error = %e, "failed to upsert entity");
            continue;
        }
        debug!(entity = %entity_name, facts = fact_list.len(), "upserted entity");
        upserted += 1;
    }

    append_extracted_marker(entry_path, "keyword")?;
    Ok(upserted)
}

fn extract_entities_via_llm(
    entry_path: &std::path::Path,
    _journal_content: &str,
    facts: &[String],
    router: DefaultRouter,
) -> Result<Vec<(String, EntityType)>> {
    let facts_text = facts.iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"Extract entities from these development session facts. Identify technologies, concepts, tools, and patterns mentioned.

Facts:
{facts_text}

Respond with ONLY a JSON object:
{{
  "entities": [
    {{"name": "Rust", "type": "Technology"}},
    {{"name": "migration patterns", "type": "Concept"}}
  ]
}}

Types: Technology, Concept, Person, Organization, Function, Module, Product, Event, Other.
Only include entities explicitly mentioned in the facts. If nothing meaningful, return empty array."#
    );

    let response = router.complete("entity_extraction", &prompt, Sensitivity::Private)?;

    let json_str = if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") { &after[..end] } else { after }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') { &response[start..=end] } else { &response }
    } else {
        &response
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str.trim())
        .context("failed to parse LLM entity extraction response")?;

    let mut entities = Vec::new();
    if let Some(arr) = parsed["entities"].as_array() {
        for item in arr {
            let name = item["name"].as_str().unwrap_or_default().trim().to_string();
            if name.is_empty() {
                continue;
            }
            let entity_type = match item["type"].as_str().unwrap_or("Technology") {
                "Technology" => EntityType::Technology,
                "Concept" => EntityType::Concept,
                "Person" => EntityType::Person,
                "Organization" => EntityType::Organization,
                "Function" => EntityType::Function,
                "Module" => EntityType::Module,
                "Product" => EntityType::Product,
                "Event" => EntityType::Event,
                _ => EntityType::Other,
            };
            entities.push((name.to_lowercase(), entity_type));
        }
    }

    if !entities.is_empty() {
        append_llm_entities_to_journal(entry_path, &entities)?;
    }

    Ok(entities)
}

fn upsert_entities(
    entities: Vec<(String, EntityType)>,
    svc: &EntityService,
    graph_db: &std::path::Path,
) -> Result<usize> {
    let mut upserted = 0usize;
    for (name, entity_type) in &entities {
        let entity = Entity::new(name.clone(), entity_type.clone(), "entity-extractor");
        if let Err(e) = svc.upsert_entity(graph_db, &entity) {
            warn!(entity = %name, error = %e, "failed to upsert entity");
            continue;
        }
        debug!(entity = %name, ?entity_type, "upserted entity via LLM");
        upserted += 1;
    }
    Ok(upserted)
}

fn match_entities(
    facts: &[String],
    known: &HashSet<String>,
) -> HashMap<String, Vec<String>> {
    let mut matched: HashMap<String, Vec<String>> = HashMap::new();

    for fact in facts {
        for entity_name in known {
            if fact.to_lowercase().contains(&entity_name.to_lowercase()) {
                matched
                    .entry(entity_name.clone())
                    .or_default()
                    .push(fact.clone());
            }
        }
    }

    matched
}

fn extract_facts_from_journal(content: &str) -> Vec<String> {
    let mut facts = Vec::new();
    let mut in_facts_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## Facts" {
            in_facts_section = true;
            continue;
        }

        if in_facts_section {
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(fact) = trimmed.strip_prefix("- ") {
                if !fact.is_empty() && fact != "_(no durable facts extracted)_" {
                    facts.push(fact.to_string());
                }
            }
        }
    }

    facts
}

fn scan_journal_entries(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read journal entries directory: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn has_extracted_marker(entry_path: &std::path::Path) -> bool {
    if JournalEntryState::has_extracted(entry_path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(entry_path) && JournalEntryState::has_extracted(entry_path)
}

fn append_extracted_marker(entry_path: &std::path::Path, source: &str) -> Result<()> {
    let state = JournalEntryState {
        extracted_at: Some(Utc::now().to_rfc3339()),
        extraction_source: Some(source.to_string()),
        ..Default::default()
    };
    state.save(entry_path)
}

fn append_llm_entities_to_journal(
    entry_path: &std::path::Path,
    entities: &[(String, EntityType)],
) -> Result<()> {
    let content = fs::read_to_string(entry_path)
        .with_context(|| format!("failed to read journal entry: {}", entry_path.display()))?;

    let mut section = String::from("\n## LLM Entities\n\n");
    for (name, entity_type) in entities {
        section.push_str(&format!("- [{entity_type:?}] {name}\n"));
    }

    let new_content = format!("{content}{section}");
    fs::write(entry_path, new_content)
        .with_context(|| format!("failed to append LLM entities: {}", entry_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_facts_from_journal() {
        let content = "---\nsession_id: test\ndate: 2026-06-20\n---\n\n## Facts\n\n- completed auth module\n- fixed login bug\n\n## Other\n\n- not a fact\n";
        let facts = extract_facts_from_journal(content);
        assert_eq!(facts.len(), 2);
        assert!(facts.contains(&"completed auth module".to_string()));
        assert!(facts.contains(&"fixed login bug".to_string()));
    }

    #[test]
    fn test_extract_facts_empty() {
        let content = "---\nsession_id: test\n---\n\n## Facts\n\n_(no durable facts extracted)_\n";
        let facts = extract_facts_from_journal(content);
        assert!(facts.is_empty());
    }

    #[test]
    fn test_match_entities() {
        let mut known = HashSet::new();
        known.insert("Rust".to_string());
        known.insert("SQLite".to_string());

        let facts = vec![
            "completed Rust auth module".to_string(),
            "fixed SQLite migration bug".to_string(),
        ];

        let matched = match_entities(&facts, &known);
        assert_eq!(matched.len(), 2);
        assert!(matched.contains_key("Rust"));
        assert!(matched.contains_key("SQLite"));
    }

    #[test]
    fn test_match_entities_case_insensitive() {
        let mut known = HashSet::new();
        known.insert("Rust".to_string());

        let facts = vec!["learned about rust async patterns".to_string()];
        let matched = match_entities(&facts, &known);
        assert_eq!(matched.len(), 1);
        assert!(matched.contains_key("Rust"));
    }

    #[test]
    fn test_has_extracted_marker_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\ndate: 2026-06-20\n---\n\ncontent\n").unwrap();

        let state = JournalEntryState {
            extracted_at: Some("2026-06-20T14:30:00Z".to_string()),
            extraction_source: Some("llm".to_string()),
            ..Default::default()
        };
        state.save(&path).unwrap();

        assert!(has_extracted_marker(&path));
    }

    #[test]
    fn test_has_extracted_marker_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();
        assert!(!has_extracted_marker(&path));
    }

    #[test]
    fn test_upsert_entities() {
        let dir = tempfile::tempdir().unwrap();
        let graph_db = dir.path().join("graph.db");
        let svc = EntityService::new();

        let entities = vec![
            ("rust".to_string(), EntityType::Technology),
            ("auth".to_string(), EntityType::Concept),
        ];

        let count = upsert_entities(entities, &svc, &graph_db).unwrap();
        assert_eq!(count, 2);

        let known = svc.load_known_entity_names(&graph_db).unwrap();
        assert!(known.contains("rust"));
        assert!(known.contains("auth"));
    }

    #[test]
    fn test_append_extracted_marker_with_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();

        append_extracted_marker(&path, "llm").unwrap();

        let state = JournalEntryState::load(&path);
        assert!(state.extracted_at.is_some());
        assert_eq!(state.extraction_source.as_deref(), Some("llm"));
    }
}
