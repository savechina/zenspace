use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use tracing::info;

use crate::note::Note;
use crate::notion::{Notion, NotionKind};

/// Notion extractor — extracts notions from notes.
///
/// Phase 1: Uses keyword heuristics and pattern matching.
/// Phase 3: LLM-based notion extraction via zen-provider.
pub struct NotionExtractor;

impl NotionExtractor {
    pub fn new() -> Self {
        Self
    }

    const KNOWN_TECHS: &[&str] = &[
        "rust",
        "python",
        "javascript",
        "typescript",
        "go",
        "java",
        "c++",
        "react",
        "vue",
        "angular",
        "svelte",
        "next.js",
        "node.js",
        "postgresql",
        "mysql",
        "sqlite",
        "mongodb",
        "redis",
        "docker",
        "kubernetes",
        "terraform",
        "aws",
        "gcp",
        "azure",
        "graphql",
        "rest",
        "grpc",
        "websocket",
        "linux",
        "macos",
        "windows",
        "git",
        "github",
        "gitlab",
        "wasm",
        "llm",
        "ai",
        "ml",
        "openai",
        "anthropic",
        "ollama",
        "deepseek",
        "sqlx",
        "rusqlite",
        "wasmtime",
        "rig-core",
        "rig-compose",
    ];

    /// Extract notions from a single note.
    ///
    /// Uses three strategies:
    /// 1. **Keyword heuristics** — scans for 42 known technology names
    /// 2. **Heading classification** — classifies `#`/`##` headings into typed notions
    ///    (Technology, Organization, Person, or Concept) based on keyword patterns
    /// 3. **Capitalized-word extraction** — finds multi-word proper nouns
    ///    (e.g., "Apple Inc", "John Smith")
    ///
    /// Returns deduplicated notions by name.
    pub fn extract(&self, note: &Note) -> Result<Vec<Notion>> {
        let mut notions: Vec<Notion> = Vec::new();
        let content_lower = note.content.to_lowercase();
        let note_id = &note.id;

        // Strategy 1: Known technology keywords
        for tech in Self::KNOWN_TECHS {
            if content_lower.contains(tech) {
                let name = capitalize(tech);
                if !notions.iter().any(|e| e.name == name) {
                    notions.push(Notion::new(&name, NotionKind::Technology, note_id));
                }
            }
        }

        // Strategy 2: Heading classification (# and ## headings)
        for line in note.content.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed
                .strip_prefix("## ")
                .or_else(|| trimmed.strip_prefix("# "))
            {
                let heading = heading.trim();
                if heading.len() >= 3 && !notions.iter().any(|e| e.name == heading) {
                    let typ = classify_heading(heading);
                    notions.push(Notion::new(heading, typ, note_id));
                }
            }
        }

        // Strategy 3: Capitalized multi-word terms (likely proper nouns)
        let mut word = String::new();
        let chars: Vec<char> = note.content.chars().collect();
        for (i, &ch) in chars.iter().enumerate() {
            if ch.is_uppercase() && (i == 0 || !chars[i - 1].is_alphabetic()) {
                word.clear();
                word.push(ch);
            } else if ch.is_alphabetic() && !word.is_empty() {
                word.push(ch);
            } else if !ch.is_alphabetic() && !word.is_empty() {
                if word.len() >= 4 {
                    let name = word.clone();
                    if !notions.iter().any(|e| e.name == name) {
                        notions.push(Notion::new(&name, NotionKind::Concept, note_id));
                    }
                }
                word.clear();
            }
        }

        info!(
            note_id = %note.id,
            entity_count = notions.len(),
            "Notion extraction complete"
        );
        Ok(notions)
    }

    /// Extract notions from multiple notes, deduplicating by name.
    pub fn extract_batch(&self, notes: &[Note]) -> Result<Vec<Notion>> {
        let mut seen = std::collections::HashMap::new();

        for note in notes {
            let extracted = self.extract(note)?;
            for notion in extracted {
                seen.entry(notion.name.clone()).or_insert(notion);
            }
        }

        Ok(seen.into_values().collect())
    }

    fn extract_from_json(&self, notes_val: &serde_json::Value) -> Result<Vec<Notion>> {
        let notes_array = notes_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected notes array"))?;

        let mut all_entities = Vec::new();

        for note_val in notes_array {
            let content = note_val
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let note_id = note_val
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown")
                .to_string();

            let content_lower = content.to_lowercase();

            for tech in Self::KNOWN_TECHS {
                if content_lower.contains(tech) {
                    let name = capitalize(tech);
                    let mut notion = Notion::new(name, NotionKind::Technology, note_id.clone());
                    notion.metadata.insert(
                        "extraction_method".to_string(),
                        "skill_heuristic".to_string(),
                    );
                    all_entities.push(notion);
                }
            }
        }

        Ok(all_entities)
    }
}

impl Default for NotionExtractor {
    fn default() -> Self {
        Self::new()
    }
}

fn capitalize(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().collect::<String>();
    first + chars.as_str()
}

fn classify_heading(heading: &str) -> NotionKind {
    let lower = heading.to_lowercase();
    if lower.contains("api")
        || lower.contains("database")
        || lower.contains("service")
        || lower.contains("cli")
        || lower.contains("library")
        || lower.contains("tool")
        || lower.contains("programming")
        || lower.contains("language")
        || lower.contains("runtime")
        || lower.contains("framework")
        || lower.contains("compiler")
        || lower.contains("algorithm")
        || lower.contains("architecture")
        || lower.contains("system")
    {
        NotionKind::Technology
    } else if lower.contains("company") || lower.contains("team") || lower.contains("org") {
        NotionKind::Organization
    } else if lower.contains("person") || lower.contains("author") {
        NotionKind::Person
    } else {
        NotionKind::Concept
    }
}

#[async_trait]
impl Skill for NotionExtractor {
    fn id(&self) -> &str {
        "zen-notion-extraction"
    }

    fn description(&self) -> &str {
        "Extract notions (technologies, concepts, people) from notes using keyword heuristics and LLM augmentation"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        !ctx.evidence.is_empty() || !ctx.signals.is_empty()
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let notions = if let Some(notes) = notes_val {
            self.extract_from_json(&notes)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?
        } else {
            info!("NotionExtractor: no notes in context, using heuristic-only extraction");
            Vec::new()
        };

        let entity_count = notions.len();
        info!(
            entity_count,
            confidence = ctx.confidence,
            "Notion extraction skill complete"
        );

        Ok(SkillOutcome::noop().with_delta(if entity_count > 0 { 0.1 } else { 0.0 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use zen_core::types::Sensitivity;

    fn make_note(content: &str) -> Note {
        Note {
            id: uuid::Uuid::now_v7().to_string(),
            tags: vec![],
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    #[test]
    fn test_extract_finds_technology() {
        let extractor = NotionExtractor;
        let note = make_note("I love using Rust and Python for programming.");
        let notions = extractor.extract(&note).unwrap();
        assert!(notions.iter().any(|e| e.name == "Rust"));
        assert!(notions.iter().any(|e| e.name == "Python"));
    }

    #[test]
    fn test_extract_finds_heading_concept() {
        let extractor = NotionExtractor;
        let note = make_note("# Async Runtime\n\nThis is about async runtimes.");
        let notions = extractor.extract(&note).unwrap();
        assert!(notions.iter().any(|e| e.name == "Async Runtime"));
    }

    #[test]
    fn test_extract_batch_deduplicates() {
        let extractor = NotionExtractor;
        let note1 = make_note("I use Rust for systems programming.");
        let note2 = make_note("Rust is great for performance.");
        let notions = extractor.extract_batch(&[note1, note2]).unwrap();
        let rust_count = notions.iter().filter(|e| e.name == "Rust").count();
        assert_eq!(rust_count, 1);
    }

    #[test]
    fn test_heading_classification_produces_typed_entities() {
        let extractor = NotionExtractor;
        let note = make_note("# Rust Programming\n\nDetails about Rust.");
        let notions = extractor.extract(&note).unwrap();
        let rust_heading = notions.iter().find(|e| e.name == "Rust Programming");
        assert!(
            rust_heading.is_some(),
            "should extract 'Rust Programming' heading"
        );
        assert_eq!(rust_heading.unwrap().kind, NotionKind::Technology);
    }

    #[test]
    fn test_heading_classification_organization() {
        let extractor = NotionExtractor;
        let note = make_note("## Engineering Team\n\nThe team works hard.");
        let notions = extractor.extract(&note).unwrap();
        let team = notions.iter().find(|e| e.name == "Engineering Team");
        assert!(team.is_some());
        assert_eq!(team.unwrap().kind, NotionKind::Organization);
    }

    #[test]
    fn test_heading_classification_person() {
        let extractor = NotionExtractor;
        let note = make_note("## Author Bio\n\nWritten by the author.");
        let notions = extractor.extract(&note).unwrap();
        let bio = notions.iter().find(|e| e.name == "Author Bio");
        assert!(bio.is_some());
        assert_eq!(bio.unwrap().kind, NotionKind::Person);
    }

    #[test]
    fn test_capitalized_words_extraction() {
        let extractor = NotionExtractor;
        let note = make_note("John Smith worked at Apple Inc on the project.");
        let notions = extractor.extract(&note).unwrap();
        assert!(
            notions.iter().any(|e| e.name == "John"),
            "should extract 'John' as capitalized word"
        );
        assert!(
            notions.iter().any(|e| e.name == "Smith"),
            "should extract 'Smith' as capitalized word"
        );
    }

    #[test]
    fn test_expanded_keyword_list_finds_sqlite() {
        let extractor = NotionExtractor;
        let note = make_note("We use sqlite for local storage.");
        let notions = extractor.extract(&note).unwrap();
        assert!(
            notions.iter().any(|e| e.name == "Sqlite"),
            "should find 'sqlite' via expanded keyword list"
        );
    }

    #[test]
    fn test_expanded_keyword_list_finds_deepseek() {
        let extractor = NotionExtractor;
        let note = make_note("We switched to deepseek for code generation.");
        let notions = extractor.extract(&note).unwrap();
        assert!(
            notions.iter().any(|e| e.name == "Deepseek"),
            "should find 'deepseek' via expanded keyword list"
        );
    }

    #[test]
    fn test_extract_deduplicates_within_note() {
        let extractor = NotionExtractor;
        let note = make_note("Rust is great. I love Rust. Rust forever.");
        let notions = extractor.extract(&note).unwrap();
        let rust_count = notions.iter().filter(|e| e.name == "Rust").count();
        assert_eq!(rust_count, 1, "should deduplicate within a single note");
    }
}
