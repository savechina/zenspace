//! 6-layer introspective Self-Model typing system.
//!
//! Models the user's identity across 6 hierarchical layers
//! (Knowledge, Skill, SocialRole, SelfConcept, Trait, Motivation),
//! stored as canonical markdown files following the Belief/Decision pattern.
//!
//! Storage: `memories/self-model/{slug}.md`

use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ─── Data types ────────────────────────────────────────────────────────

/// The 6-layer introspective typing hierarchy for self-model items.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfModelLayer {
    /// Declarative knowledge (e.g., "knows GTD 5 steps").
    Knowledge,
    /// Applied ability (e.g., "writes Rust async").
    Skill,
    /// Social/role identity (e.g., "architect", "father").
    SocialRole,
    /// Self-concept and identity beliefs (e.g., "long-termist").
    SelfConcept,
    /// Behavioral trait (e.g., "honest", "over-reserved").
    Trait,
    /// Core motivation (e.g., "achievement-driven").
    Motivation,
}

impl fmt::Display for SelfModelLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SelfModelLayer::Knowledge => "knowledge",
            SelfModelLayer::Skill => "skill",
            SelfModelLayer::SocialRole => "social_role",
            SelfModelLayer::SelfConcept => "self_concept",
            SelfModelLayer::Trait => "trait",
            SelfModelLayer::Motivation => "motivation",
        };
        write!(f, "{s}")
    }
}

impl FromStr for SelfModelLayer {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "knowledge" => Ok(SelfModelLayer::Knowledge),
            "skill" => Ok(SelfModelLayer::Skill),
            "social_role" => Ok(SelfModelLayer::SocialRole),
            "self_concept" => Ok(SelfModelLayer::SelfConcept),
            "trait" => Ok(SelfModelLayer::Trait),
            "motivation" => Ok(SelfModelLayer::Motivation),
            _ => Err(anyhow::anyhow!("invalid SelfModelLayer: {s}")),
        }
    }
}

/// A single self-model item across any of the 6 layers.
///
/// Uses a flat struct with Optional fields — layer-specific fields are only
/// populated for the relevant layer (consistent with the Decision pattern).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelfModelItem {
    /// Unique identifier (slug-based).
    pub id: String,
    /// Which of the 6 layers this item belongs to.
    pub layer: SelfModelLayer,
    /// Human-readable name (e.g., "writes Rust async").
    pub name: String,
    /// Detailed description of this self-model item.
    pub description: String,
    /// Domain categorization (e.g., "programming", "family", "career").
    pub domain: String,

    // Layer-specific fields (all Optional — used per layer)
    /// Knowledge layer: is this explicit/declarative knowledge?
    pub is_explicit: Option<bool>,
    /// Skill layer: roles this skill is sufficient for.
    pub sufficient_for: Vec<String>,
    /// Skill layer: roles where this skill is necessary.
    pub necessary_for: Vec<String>,
    /// SocialRole layer: how controllable this role is (0.0-1.0).
    pub controllability: Option<f64>,
    /// SelfConcept layer: auto-computed humility score (0.0-1.0).
    pub humility_score: Option<f64>,
    /// Trait layer: how many paths remain open (Munger optionality).
    pub optionality_count: Option<u32>,
    /// Motivation layer: the core pursuit driving this motivation.
    pub core_pursuit: Option<String>,

    // Common metadata
    /// Source of this item: "fact" | "reflection" | "antipattern" | "belief" | "manual".
    pub source: String,
    /// Confidence in this item (0.0-1.0).
    pub confidence: f64,
    /// When this item was created.
    pub created_at: DateTime<Utc>,
    /// When this item was last updated.
    pub updated_at: DateTime<Utc>,
    /// IDs of related beliefs, decisions, or reflections.
    pub evidence_refs: Vec<String>,
}

impl SelfModelItem {
    /// Create a new self-model item with sensible defaults.
    pub fn new(id: String, layer: SelfModelLayer, name: String, domain: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            layer,
            name,
            description: String::new(),
            domain,
            is_explicit: None,
            sufficient_for: Vec::new(),
            necessary_for: Vec::new(),
            controllability: None,
            humility_score: None,
            optionality_count: None,
            core_pursuit: None,
            source: "manual".to_string(),
            confidence: 0.5,
            created_at: now,
            updated_at: now,
            evidence_refs: Vec::new(),
        }
    }

    /// Generate a URL-safe slug from the item name for filename generation.
    pub fn slug(&self) -> String {
        slugify_name(&self.name)
    }
}

// ─── Slugify ───────────────────────────────────────────────────────────

/// Slugify a name into a filesystem-safe identifier.
pub fn slugify_name(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .chars()
        .take(60)
        .collect()
}

// ─── File persistence ──────────────────────────────────────────────────

impl SelfModelItem {
    /// Serialize item to markdown format with YAML frontmatter + body.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!("layer: \"{}\"\n", self.layer));
        md.push_str(&format!("name: \"{}\"\n", self.name.replace('"', "\\\"")));
        md.push_str(&format!("domain: {}\n", self.domain));
        md.push_str(&format!("source: {}\n", self.source));
        md.push_str(&format!("confidence: {}\n", self.confidence));
        md.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        md.push_str(&format!("updated_at: {}\n", self.updated_at.to_rfc3339()));

        // Layer-specific optional fields
        if let Some(is_explicit) = self.is_explicit {
            md.push_str(&format!("is_explicit: {is_explicit}\n"));
        }
        if let Some(controllability) = self.controllability {
            md.push_str(&format!("controllability: {controllability}\n"));
        }
        if let Some(humility_score) = self.humility_score {
            md.push_str(&format!("humility_score: {humility_score}\n"));
        }
        if let Some(optionality_count) = self.optionality_count {
            md.push_str(&format!("optionality_count: {optionality_count}\n"));
        }
        if let Some(ref core_pursuit) = self.core_pursuit {
            md.push_str(&format!("core_pursuit: \"{core_pursuit}\"\n"));
        }
        if !self.sufficient_for.is_empty() {
            md.push_str(&format!(
                "sufficient_for: [{}]\n",
                self.sufficient_for
                    .iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.necessary_for.is_empty() {
            md.push_str(&format!(
                "necessary_for: [{}]\n",
                self.necessary_for
                    .iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.evidence_refs.is_empty() {
            md.push_str(&format!(
                "evidence_refs: [{}]\n",
                self.evidence_refs
                    .iter()
                    .map(|s| format!("\"{s}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        md.push_str("---\n\n");
        md.push_str(&format!("# {}\n\n", self.name));
        if !self.description.is_empty() {
            md.push_str(&format!("{}\n", self.description));
        }
        md
    }

    /// Save item to `dir/{slug}.md`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create self-model dir: {}", dir.display()))?;
        let path = dir.join(format!("{}.md", self.slug()));
        let content = self.to_markdown();
        fs::write(&path, content)
            .with_context(|| format!("failed to write self-model file: {}", path.display()))?;
        Ok(())
    }

    /// Load all self-model items from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<SelfModelItem>> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::from_file(&path) {
                    Ok(item) => items.push(item),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse self-model file, skipping"
                        );
                    }
                }
            }
        }
        Ok(items)
    }

    /// Parse a self-model item from a markdown file.
    pub fn from_file(path: &Path) -> Result<SelfModelItem> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read self-model file: {}", path.display()))?;
        Self::from_markdown(&content)
    }

    /// Parse self-model item from markdown string (frontmatter + body).
    pub fn from_markdown(content: &str) -> Result<SelfModelItem> {
        let fm = extract_frontmatter(content)?;
        let id = parse_yaml_field(&fm, "id").ok_or_else(|| anyhow::anyhow!("missing id field"))?;
        let layer_str =
            parse_yaml_field(&fm, "layer").ok_or_else(|| anyhow::anyhow!("missing layer field"))?;
        let layer = SelfModelLayer::from_str(layer_str.trim_matches('"'))?;
        let name = parse_yaml_field(&fm, "name")
            .map(|s| s.trim_matches('"').to_string())
            .unwrap_or_default();
        let domain = parse_yaml_field(&fm, "domain").unwrap_or_else(|| "uncategorized".to_string());
        let source = parse_yaml_field(&fm, "source").unwrap_or_else(|| "manual".to_string());
        let confidence: f64 = parse_yaml_field(&fm, "confidence")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.5);
        let created_at = parse_yaml_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let updated_at = parse_yaml_field(&fm, "updated_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        // Layer-specific optional fields
        let is_explicit = parse_yaml_field(&fm, "is_explicit").map(|s| s == "true");
        let controllability = parse_yaml_field(&fm, "controllability").and_then(|s| s.parse().ok());
        let humility_score = parse_yaml_field(&fm, "humility_score").and_then(|s| s.parse().ok());
        let optionality_count =
            parse_yaml_field(&fm, "optionality_count").and_then(|s| s.parse().ok());
        let core_pursuit =
            parse_yaml_field(&fm, "core_pursuit").map(|s| s.trim_matches('"').to_string());
        let sufficient_for = parse_yaml_array(&fm, "sufficient_for");
        let necessary_for = parse_yaml_array(&fm, "necessary_for");
        let evidence_refs = parse_yaml_array(&fm, "evidence_refs");

        // Extract description from body (everything after frontmatter + heading)
        let body = extract_body(content).unwrap_or_default();
        let description = body
            .lines()
            .skip_while(|l| l.starts_with('#'))
            .skip_while(|l| l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        Ok(SelfModelItem {
            id,
            layer,
            name,
            description,
            domain,
            is_explicit,
            sufficient_for,
            necessary_for,
            controllability,
            humility_score,
            optionality_count,
            core_pursuit,
            source,
            confidence,
            created_at,
            updated_at,
            evidence_refs,
        })
    }
}

// ─── Frontmatter parsing helpers ───────────────────────────────────────

/// Extract the YAML frontmatter block (between first two `---` lines).
fn extract_frontmatter(content: &str) -> Result<String> {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or("").trim();
    if first != "---" {
        anyhow::bail!("missing frontmatter opening ---");
    }
    let mut fm = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Ok(fm);
        }
        fm.push_str(line);
        fm.push('\n');
    }
    anyhow::bail!("missing frontmatter closing ---");
}

/// Parse a simple `key: value` line from frontmatter. Returns the trimmed value.
fn parse_yaml_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Parse a YAML array field like `key: ["a", "b"]` or `key: []`.
fn parse_yaml_array(frontmatter: &str, key: &str) -> Vec<String> {
    let val = match parse_yaml_field(frontmatter, key) {
        Some(v) => v,
        None => return Vec::new(),
    };
    // Handle empty array
    let trimmed = val.trim();
    if trimmed == "[]" {
        return Vec::new();
    }
    // Strip brackets and split by comma
    let inner = trimmed.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract everything after the frontmatter closing `---`.
fn extract_body(content: &str) -> Result<String> {
    let mut lines = content.lines();
    // Skip opening ---
    lines.next();
    let mut past_frontmatter = false;
    let mut body = String::new();
    for line in lines {
        if !past_frontmatter {
            if line.trim() == "---" {
                past_frontmatter = true;
            }
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    Ok(body.trim().to_string())
}

// ─── Humility score computation ────────────────────────────────────────

/// Compute humility score from historical decisions.
///
/// Returns `None` if fewer than `min_decisions` have outcomes.
/// `humility_score` = (decisions where outcome matched expectation) /
/// (total decisions with outcomes).
///
/// Per DESIGN.md §5.2: "historical Decision outcome match rate."
/// Per Risk #7: "Require minimum 10 decisions before triggering."
pub fn compute_humility_score(decisions_dir: &Path, min_decisions: usize) -> Option<f64> {
    let decisions = match crate::decision::Decision::load_all(decisions_dir) {
        Ok(d) => d,
        Err(_) => return None,
    };

    let with_outcome: Vec<_> = decisions.iter().filter(|d| d.outcome.is_some()).collect();

    if with_outcome.len() < min_decisions {
        return None;
    }

    let total = with_outcome.len() as f64;
    let matched = with_outcome
        .iter()
        .filter(|d| {
            matches!(
                d.outcome.as_ref().map(|o| &o.result),
                Some(crate::decision::OutcomeResult::Success)
            )
        })
        .count() as f64;

    Some(matched / total)
}

// ─── Aggregation helpers ───────────────────────────────────────────────

/// Filter self-model items by layer.
pub fn items_by_layer(items: &[SelfModelItem], layer: SelfModelLayer) -> Vec<&SelfModelItem> {
    items.iter().filter(|i| i.layer == layer).collect()
}

/// Get items sorted by confidence (descending), take top N.
pub fn top_by_confidence(items: &[SelfModelItem], n: usize) -> Vec<&SelfModelItem> {
    let mut sorted: Vec<&SelfModelItem> = items.iter().collect();
    sorted.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.into_iter().take(n).collect()
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_self_model_layer_display() {
        assert_eq!(SelfModelLayer::Knowledge.to_string(), "knowledge");
        assert_eq!(SelfModelLayer::Skill.to_string(), "skill");
        assert_eq!(SelfModelLayer::SocialRole.to_string(), "social_role");
        assert_eq!(SelfModelLayer::SelfConcept.to_string(), "self_concept");
        assert_eq!(SelfModelLayer::Trait.to_string(), "trait");
        assert_eq!(SelfModelLayer::Motivation.to_string(), "motivation");
    }

    #[test]
    fn test_self_model_layer_from_str() {
        assert_eq!(
            SelfModelLayer::from_str("knowledge").unwrap(),
            SelfModelLayer::Knowledge
        );
        assert_eq!(
            SelfModelLayer::from_str("skill").unwrap(),
            SelfModelLayer::Skill
        );
        assert_eq!(
            SelfModelLayer::from_str("social_role").unwrap(),
            SelfModelLayer::SocialRole
        );
        assert_eq!(
            SelfModelLayer::from_str("self_concept").unwrap(),
            SelfModelLayer::SelfConcept
        );
        assert_eq!(
            SelfModelLayer::from_str("trait").unwrap(),
            SelfModelLayer::Trait
        );
        assert_eq!(
            SelfModelLayer::from_str("motivation").unwrap(),
            SelfModelLayer::Motivation
        );
    }

    #[test]
    fn test_self_model_layer_from_str_invalid() {
        assert!(SelfModelLayer::from_str("invalid").is_err());
        assert!(SelfModelLayer::from_str("").is_err());
        assert!(SelfModelLayer::from_str("Knowledge").is_err());
    }

    #[test]
    fn test_self_model_item_new_defaults() {
        let item = SelfModelItem::new(
            "test-id".into(),
            SelfModelLayer::Knowledge,
            "Test Knowledge".into(),
            "programming".into(),
        );
        assert_eq!(item.id, "test-id");
        assert_eq!(item.layer, SelfModelLayer::Knowledge);
        assert_eq!(item.name, "Test Knowledge");
        assert_eq!(item.domain, "programming");
        assert!(item.description.is_empty());
        assert_eq!(item.is_explicit, None);
        assert!(item.sufficient_for.is_empty());
        assert!(item.necessary_for.is_empty());
        assert_eq!(item.controllability, None);
        assert_eq!(item.humility_score, None);
        assert_eq!(item.optionality_count, None);
        assert_eq!(item.core_pursuit, None);
        assert_eq!(item.source, "manual");
        assert_eq!(item.confidence, 0.5);
        assert!(item.evidence_refs.is_empty());
    }

    #[test]
    fn test_self_model_item_slug() {
        let item = SelfModelItem::new(
            "test".into(),
            SelfModelLayer::Skill,
            "Writes Rust Async".into(),
            "programming".into(),
        );
        assert_eq!(item.slug(), "writes-rust-async");
    }

    #[test]
    fn test_to_markdown_knowledge_layer() {
        let mut item = SelfModelItem::new(
            "k-001".into(),
            SelfModelLayer::Knowledge,
            "GTD 5 Steps".into(),
            "productivity".into(),
        );
        item.is_explicit = Some(true);
        item.description = "Knows the 5-step GTD workflow.".into();
        item.source = "fact".into();
        item.confidence = 0.8;

        let md = item.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: k-001"));
        assert!(md.contains("layer: \"knowledge\""));
        assert!(md.contains("is_explicit: true"));
        assert!(md.contains("confidence: 0.8"));
        assert!(md.contains("# GTD 5 Steps"));
        assert!(md.contains("Knows the 5-step GTD workflow."));
    }

    #[test]
    fn test_to_markdown_skill_layer() {
        let mut item = SelfModelItem::new(
            "s-001".into(),
            SelfModelLayer::Skill,
            "Writes Rust Async".into(),
            "programming".into(),
        );
        item.sufficient_for = vec!["architect".into(), "engineer".into()];
        item.necessary_for = vec!["systems-engineer".into()];

        let md = item.to_markdown();
        assert!(md.contains("sufficient_for: [\"architect\", \"engineer\"]"));
        assert!(md.contains("necessary_for: [\"systems-engineer\"]"));
    }

    #[test]
    fn test_to_markdown_social_role_layer() {
        let mut item = SelfModelItem::new(
            "sr-001".into(),
            SelfModelLayer::SocialRole,
            "Architect".into(),
            "career".into(),
        );
        item.controllability = Some(0.7);

        let md = item.to_markdown();
        assert!(md.contains("controllability: 0.7"));
    }

    #[test]
    fn test_to_markdown_minimal() {
        let item = SelfModelItem::new(
            "m-001".into(),
            SelfModelLayer::Trait,
            "Honest".into(),
            "personality".into(),
        );
        let md = item.to_markdown();
        // Optional fields should not appear
        assert!(!md.contains("is_explicit"));
        assert!(!md.contains("controllability"));
        assert!(!md.contains("humility_score"));
        assert!(!md.contains("optionality_count"));
        assert!(!md.contains("core_pursuit"));
        assert!(!md.contains("sufficient_for"));
        assert!(!md.contains("necessary_for"));
        assert!(!md.contains("evidence_refs"));
    }

    #[test]
    fn test_from_markdown_roundtrip() {
        let mut item = SelfModelItem::new(
            "rt-test".into(),
            SelfModelLayer::Skill,
            "Writes Rust Async".into(),
            "programming".into(),
        );
        item.description = "Async Rust programming skill.".into();
        item.is_explicit = None;
        item.sufficient_for = vec!["architect".into()];
        item.necessary_for = vec!["systems-engineer".into()];
        item.humility_score = Some(0.65);
        item.source = "fact".into();
        item.confidence = 0.9;
        item.evidence_refs = vec!["belief-001".into()];

        let md = item.to_markdown();
        let parsed = SelfModelItem::from_markdown(&md).unwrap();
        assert_eq!(parsed.id, "rt-test");
        assert_eq!(parsed.layer, SelfModelLayer::Skill);
        assert_eq!(parsed.name, "Writes Rust Async");
        assert_eq!(parsed.domain, "programming");
        assert_eq!(parsed.description, "Async Rust programming skill.");
        assert_eq!(parsed.sufficient_for, vec!["architect"]);
        assert_eq!(parsed.necessary_for, vec!["systems-engineer"]);
        assert_eq!(parsed.humility_score, Some(0.65));
        assert_eq!(parsed.source, "fact");
        assert_eq!(parsed.confidence, 0.9);
        assert_eq!(parsed.evidence_refs, vec!["belief-001"]);
    }

    #[test]
    fn test_from_markdown_missing_optionals() {
        let md = r#"---
id: test
layer: "knowledge"
name: "Test Item"
domain: test
source: manual
confidence: 0.5
created_at: 2026-06-26T00:00:00Z
updated_at: 2026-06-26T00:00:00Z
---

# Test Item
"#;
        let item = SelfModelItem::from_markdown(md).unwrap();
        assert_eq!(item.is_explicit, None);
        assert_eq!(item.controllability, None);
        assert_eq!(item.humility_score, None);
        assert_eq!(item.optionality_count, None);
        assert_eq!(item.core_pursuit, None);
        assert!(item.sufficient_for.is_empty());
        assert!(item.necessary_for.is_empty());
        assert!(item.evidence_refs.is_empty());
    }

    #[test]
    fn test_save_and_load() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("self-model");

        let mut item = SelfModelItem::new(
            "save-test".into(),
            SelfModelLayer::Motivation,
            "Achievement Driven".into(),
            "career".into(),
        );
        item.core_pursuit = Some("mastery".into());
        item.description = "Driven by achievement.".into();
        item.save(&dir).unwrap();

        let loaded = SelfModelItem::load_all(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        let li = &loaded[0];
        assert_eq!(li.id, "save-test");
        assert_eq!(li.layer, SelfModelLayer::Motivation);
        assert_eq!(li.name, "Achievement Driven");
        assert_eq!(li.core_pursuit, Some("mastery".into()));
        assert_eq!(li.description, "Driven by achievement.");
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let items = SelfModelItem::load_all(tmp.path()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_load_all_nonexistent_dir() {
        let items = SelfModelItem::load_all(Path::new("/nonexistent/path/xyz")).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_load_all_skips_invalid() {
        let tmp = tempdir().unwrap();
        // Write a valid item
        let item = SelfModelItem::new(
            "valid".into(),
            SelfModelLayer::Knowledge,
            "Valid Item".into(),
            "test".into(),
        );
        item.save(tmp.path()).unwrap();

        // Write an invalid file
        fs::write(tmp.path().join("invalid.md"), "not a frontmatter file").unwrap();

        let loaded = SelfModelItem::load_all(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "valid");
    }

    #[test]
    fn test_compute_humility_score_insufficient() {
        let tmp = tempdir().unwrap();
        let decisions_dir = tmp.path().join("decisions");
        fs::create_dir_all(&decisions_dir).unwrap();

        // Create fewer than 10 decisions with outcomes
        for i in 0..5 {
            let mut d = crate::decision::Decision::new(
                format!("d-{i}"),
                format!("Decision {i}"),
                "test".into(),
            );
            d.outcome = Some(crate::decision::Outcome {
                result: crate::decision::OutcomeResult::Success,
                notes: "done".into(),
                recorded_at: Utc::now(),
            });
            d.save(&decisions_dir).unwrap();
        }

        let score = compute_humility_score(&decisions_dir, 10);
        assert_eq!(score, None);
    }

    #[test]
    fn test_compute_humility_score_sufficient() {
        let tmp = tempdir().unwrap();
        let decisions_dir = tmp.path().join("decisions");
        fs::create_dir_all(&decisions_dir).unwrap();

        // Create 10 decisions: 7 success, 3 failure
        for i in 0..10 {
            let mut d = crate::decision::Decision::new(
                format!("d-{i}"),
                format!("Decision {i}"),
                "test".into(),
            );
            d.outcome = Some(crate::decision::Outcome {
                result: if i < 7 {
                    crate::decision::OutcomeResult::Success
                } else {
                    crate::decision::OutcomeResult::Failure
                },
                notes: "done".into(),
                recorded_at: Utc::now(),
            });
            d.save(&decisions_dir).unwrap();
        }

        let score = compute_humility_score(&decisions_dir, 10).unwrap();
        assert!((score - 0.7).abs() < 0.001, "expected ~0.7, got {score}");
    }

    #[test]
    fn test_compute_humility_score_no_decisions_dir() {
        let score = compute_humility_score(Path::new("/nonexistent/path"), 10);
        assert_eq!(score, None);
    }

    #[test]
    fn test_items_by_layer() {
        let items = vec![
            SelfModelItem::new(
                "k1".into(),
                SelfModelLayer::Knowledge,
                "K1".into(),
                "d".into(),
            ),
            SelfModelItem::new("s1".into(), SelfModelLayer::Skill, "S1".into(), "d".into()),
            SelfModelItem::new(
                "k2".into(),
                SelfModelLayer::Knowledge,
                "K2".into(),
                "d".into(),
            ),
        ];
        let knowledge = items_by_layer(&items, SelfModelLayer::Knowledge);
        assert_eq!(knowledge.len(), 2);
        assert!(
            knowledge
                .iter()
                .all(|i| i.layer == SelfModelLayer::Knowledge)
        );

        let skills = items_by_layer(&items, SelfModelLayer::Skill);
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn test_top_by_confidence() {
        let mut item1 = SelfModelItem::new(
            "low".into(),
            SelfModelLayer::Trait,
            "Low".into(),
            "d".into(),
        );
        item1.confidence = 0.3;
        let mut item2 = SelfModelItem::new(
            "high".into(),
            SelfModelLayer::Trait,
            "High".into(),
            "d".into(),
        );
        item2.confidence = 0.9;
        let mut item3 = SelfModelItem::new(
            "mid".into(),
            SelfModelLayer::Trait,
            "Mid".into(),
            "d".into(),
        );
        item3.confidence = 0.6;

        let items = vec![item1, item2, item3];
        let top = top_by_confidence(&items, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].id, "high");
        assert_eq!(top[1].id, "mid");
    }

    #[test]
    fn test_slugify_name() {
        assert_eq!(
            slugify_name("JWT with refresh rotation"),
            "jwt-with-refresh-rotation"
        );
        assert_eq!(slugify_name("  spaces  "), "spaces");
        let long =
            slugify_name("A very long name that exceeds the sixty character limit for slugs");
        assert_eq!(long.len(), 60);
    }

    #[test]
    fn test_from_markdown_invalid_content() {
        let result = SelfModelItem::from_markdown("not a frontmatter file");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_markdown_missing_frontmatter_close() {
        let result = SelfModelItem::from_markdown("---\nno closing");
        assert!(result.is_err());
    }
}
