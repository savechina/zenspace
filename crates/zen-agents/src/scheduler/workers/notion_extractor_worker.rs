use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::sanitize::InputSanitizer;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouterExt};
use zen_vault::notion::{Notion, NotionKind, NotionService};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

const TECH_KEYWORDS: &[&str] = &[
    "Rust",
    "Python",
    "TypeScript",
    "JavaScript",
    "CLIP",
    "LLM",
    "GPT",
    "OpenAI",
    "Anthropic",
    "DeepSeek",
    "Gemini",
    "Tokio",
    "async",
    "sqlite",
    "SQLite",
    "FTS5",
    "vector",
    "embedding",
];

const MIN_CONTENT_LEN: usize = 20;

pub struct NotionExtractorWorker {
    scheduled: Option<&'static str>,
}

impl NotionExtractorWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for NotionExtractorWorker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for NotionExtractorWorker {
    fn id(&self) -> &'static str {
        "notion-extractor"
    }

    fn description(&self) -> &'static str {
        "Scan journal entries, extract notions via LLM with keyword fallback, upsert to state.db"
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
                llm_cost_usd: 0.0,
            });
        }

        let entries = scan_journal_entries(&journal_dir)?;
        if entries.is_empty() {
            debug!(
                "no journal entries found in {}, nothing to extract",
                journal_dir.display()
            );
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                llm_cost_usd: 0.0,
            });
        }

        let state_db = paths.data().join("state.db");
        let client = match zen_vault::SqliteClient::open(&state_db).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to open state.db, notion extraction aborted");
                return Ok(WorkerReport {
                    worker_id: self.id().to_string(),
                    success: false,
                    fact_count: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    llm_cost_usd: 0.0,
                });
            }
        };
        let svc = NotionService::new();

        let mut known = svc
            .load_known_notion_names(&client)
            .await
            .unwrap_or_default();
        for kw in TECH_KEYWORDS {
            known.insert((*kw).to_string());
        }

        let router: Option<DefaultRouter> = match load_config() {
            Ok(c) => Some(DefaultRouter::from_agentic(c)),
            Err(e) => {
                warn!(error = %e, "failed to load config for LLM notion extraction, falling back to keyword-only");
                None
            }
        };

        let mut processed = 0usize;
        let mut total_entities = 0usize;

        for entry_path in &entries {
            if has_extracted_marker(entry_path) {
                continue;
            }

            match process_entry(entry_path, &svc, &client, &known, router.clone()).await {
                Ok(count) => {
                    total_entities += count;
                    processed += 1;
                    if count > 0 {
                        info!(path = %entry_path.display(), notions = count, "notions extracted from journal entry");
                    }
                }
                Err(e) => {
                    warn!(path = %entry_path.display(), error = %e, "failed to extract notions from journal entry");
                }
            }
        }

        if processed > 0 {
            info!(
                processed,
                notions = total_entities,
                "notion-extractor tick complete"
            );
        } else {
            debug!(
                "notion-extractor tick: {} entries scanned, none needed processing (all previously extracted or empty)",
                entries.len()
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_entities,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

async fn process_entry(
    entry_path: &std::path::Path,
    svc: &NotionService,
    client: &zen_vault::SqliteClient,
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
                info!(path = %entry_path.display(), count = llm_entities.len(), "LLM notion extraction succeeded");
                let count = upsert_entities(llm_entities, svc, client).await?;
                append_extracted_marker(entry_path, "llm")?;
                return Ok(count);
            }
            Ok(_) => {
                debug!(path = %entry_path.display(), "LLM returned no notions, falling back to keyword");
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
    for (notion_name, fact_list) in &matched {
        let canonical = notion_name.to_lowercase();
        let notion = Notion::new(canonical, NotionKind::Technology, "notion-extractor");
        if let Err(e) = svc.upsert_entity(client, &notion).await {
            warn!(notion = %notion_name, error = %e, "failed to upsert notion");
            continue;
        }
        debug!(notion = %notion_name, facts = fact_list.len(), "upserted notion");
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
) -> Result<Vec<(String, NotionKind)>> {
    let facts_text = facts
        .iter()
        .map(|f| format!("- {f}"))
        .collect::<Vec<_>>()
        .join("\n");

    let sanitizer = InputSanitizer::new();
    let facts_text = sanitizer.strip_dangerous_patterns(&facts_text);

    let prompt = format!(
        r#"Extract notions from these development session facts. Identify technologies, concepts, tools, and patterns mentioned.

Facts:
{facts_text}

Respond with ONLY a JSON object:
{{
  "notions": [
    {{"name": "Rust", "type": "Technology"}},
    {{"name": "migration patterns", "type": "Concept"}}
  ]
}}

Types: Technology, Concept, Person, Organization, Function, Module, Product, Event, Other.
Only include notions explicitly mentioned in the facts. If nothing meaningful, return empty array."#
    );

    let response = router.complete("notion_extraction", &prompt, Sensitivity::Private)?;

    let json_str = if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") {
            &after[..end]
        } else {
            after
        }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            &response
        }
    } else {
        &response
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str.trim())
        .context("failed to parse LLM notion extraction response")?;

    let mut notions = Vec::new();
    if let Some(arr) = parsed["notions"].as_array() {
        for item in arr {
            let name = item["name"].as_str().unwrap_or_default().trim().to_string();
            if name.is_empty() {
                continue;
            }
            let kind = match item["type"].as_str().unwrap_or("Technology") {
                "Technology" => NotionKind::Technology,
                "Concept" => NotionKind::Concept,
                "Person" => NotionKind::Person,
                "Organization" => NotionKind::Organization,
                "Function" => NotionKind::Function,
                "Module" => NotionKind::Module,
                "Product" => NotionKind::Product,
                "Event" => NotionKind::Event,
                _ => NotionKind::Other,
            };
            notions.push((name.to_lowercase(), kind));
        }
    }

    if !notions.is_empty() {
        append_llm_entities_to_journal(entry_path, &notions)?;
    }

    Ok(notions)
}

async fn upsert_entities(
    notions: Vec<(String, NotionKind)>,
    svc: &NotionService,
    client: &zen_vault::SqliteClient,
) -> Result<usize> {
    let mut upserted = 0usize;
    for (name, kind) in &notions {
        let notion = Notion::new(name.clone(), kind.clone(), "notion-extractor");
        if let Err(e) = svc.upsert_entity(client, &notion).await {
            warn!(notion = %name, error = %e, "failed to upsert notion");
            continue;
        }
        debug!(notion = %name, ?kind, "upserted notion via LLM");
        upserted += 1;
    }
    Ok(upserted)
}

fn match_entities(facts: &[String], known: &HashSet<String>) -> HashMap<String, Vec<String>> {
    let mut matched: HashMap<String, Vec<String>> = HashMap::new();

    for fact in facts {
        for notion_name in known {
            if fact.to_lowercase().contains(&notion_name.to_lowercase()) {
                matched
                    .entry(notion_name.clone())
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
            if let Some(fact) = trimmed.strip_prefix("- ")
                && !fact.is_empty()
                && fact != "_(no durable facts extracted)_"
            {
                facts.push(fact.to_string());
            }
        }
    }

    facts
}

fn scan_journal_entries(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| {
        format!(
            "failed to read journal entries directory: {}",
            dir.display()
        )
    })? {
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
    JournalEntryState::migrate_from_frontmatter(entry_path)
        && JournalEntryState::has_extracted(entry_path)
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
    notions: &[(String, NotionKind)],
) -> Result<()> {
    let content = fs::read_to_string(entry_path)
        .with_context(|| format!("failed to read journal entry: {}", entry_path.display()))?;

    let mut section = String::from("\n## LLM Entities\n\n");
    for (name, kind) in notions {
        section.push_str(&format!("- [{kind:?}] {name}\n"));
    }

    let new_content = format!("{content}{section}");
    fs::write(entry_path, new_content)
        .with_context(|| format!("failed to append LLM notions: {}", entry_path.display()))?;

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
        fs::write(
            &path,
            "---\nsession_id: test\ndate: 2026-06-20\n---\n\ncontent\n",
        )
        .unwrap();

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

    #[tokio::test]
    async fn test_upsert_entities() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = dir.path().join("state.db");
        let client = zen_vault::SqliteClient::open(&state_db).await.unwrap();
        let svc = NotionService::new();

        let notions = vec![
            ("rust".to_string(), NotionKind::Technology),
            ("auth".to_string(), NotionKind::Concept),
        ];

        let count = upsert_entities(notions, &svc, &client).await.unwrap();
        assert_eq!(count, 2);

        let known = svc.load_known_notion_names(&client).await.unwrap();
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
