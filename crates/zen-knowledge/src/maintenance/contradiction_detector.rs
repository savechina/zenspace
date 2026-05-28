use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedContradiction {
    pub claim_a: String,
    pub claim_b: String,
    pub source_a: String,
    pub source_b: String,
    pub contradiction_type: ContradictionType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContradictionType {
    Negation,
    ValueConflict,
    Temporal,
}

impl std::fmt::Display for ContradictionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContradictionType::Negation => write!(f, "negation"),
            ContradictionType::ValueConflict => write!(f, "value_conflict"),
            ContradictionType::Temporal => write!(f, "temporal"),
        }
    }
}

const CLAIM_KEYWORDS: &[&str] = &[
    " is ", " are ", " was ", " were ", " should ", " must ", " never ", " always ", " uses ",
    " can ", " cannot ", " may ", " will ",
];

const NEGATION_INDICATORS: &[&str] = &[" not ", " never ", " no ", " cannot "];

pub struct ContradictionDetectorSkill {
    wiki_dir: PathBuf,
}

impl ContradictionDetectorSkill {
    pub fn new(wiki_dir: PathBuf) -> Self {
        Self { wiki_dir }
    }

    pub fn detect(&self, notes: &[serde_json::Value]) -> Result<Vec<DetectedContradiction>> {
        let claims = self.extract_claims(notes);
        let contradictions = self.find_contradictions(&claims);

        if !contradictions.is_empty() {
            let _ = self.write_contradictions(&contradictions);
        }

        info!(
            claim_count = claims.len(),
            contradiction_count = contradictions.len(),
            "ContradictionDetectorSkill: detection complete"
        );

        Ok(contradictions)
    }

    fn extract_claims(&self, notes: &[serde_json::Value]) -> Vec<(String, String)> {
        let mut claims = Vec::new();

        for note in notes {
            let content = note
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");

            let note_id = note
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("unknown")
                .to_string();

            for sentence in self.split_sentences(content) {
                if self.is_factual_claim(&sentence) {
                    let normalized = self.normalize(&sentence);
                    if !normalized.is_empty() {
                        claims.push((normalized, note_id.clone()));
                    }
                }
            }
        }

        claims
    }

    fn find_contradictions(&self, claims: &[(String, String)]) -> Vec<DetectedContradiction> {
        let mut contradictions = Vec::new();
        let mut seen = HashSet::new();

        for i in 0..claims.len() {
            for j in (i + 1)..claims.len() {
                let (text_a, source_a) = &claims[i];
                let (text_b, source_b) = &claims[j];

                if source_a == source_b {
                    continue;
                }

                let dedup_key = {
                    let (first, first_src, second, second_src) = if source_a <= source_b {
                        (text_a.as_str(), source_a.as_str(), text_b.as_str(), source_b.as_str())
                    } else {
                        (text_b.as_str(), source_b.as_str(), text_a.as_str(), source_a.as_str())
                    };
                    (first.to_string(), second.to_string(), first_src.to_string(), second_src.to_string())
                };

                if !seen.insert(dedup_key) {
                    continue;
                }

                if let Some(c_type) = self.check_contradiction(text_a, text_b) {
                    let c = if source_a <= source_b {
                        DetectedContradiction {
                            claim_a: text_a.clone(),
                            claim_b: text_b.clone(),
                            source_a: source_a.clone(),
                            source_b: source_b.clone(),
                            contradiction_type: c_type,
                        }
                    } else {
                        DetectedContradiction {
                            claim_a: text_b.clone(),
                            claim_b: text_a.clone(),
                            source_a: source_b.clone(),
                            source_b: source_a.clone(),
                            contradiction_type: c_type,
                        }
                    };
                    contradictions.push(c);
                }
            }
        }

        contradictions
    }

    fn check_contradiction(&self, a: &str, b: &str) -> Option<ContradictionType> {
        if self.is_negation_contradiction(a, b) {
            return Some(ContradictionType::Negation);
        }

        if self.is_value_conflict(a, b) {
            return Some(ContradictionType::ValueConflict);
        }

        if self.is_temporal_contradiction(a, b) {
            return Some(ContradictionType::Temporal);
        }

        None
    }

    fn is_negation_contradiction(&self, claim_a: &str, claim_b: &str) -> bool {
        let a_has_neg = NEGATION_INDICATORS.iter().any(|&kw| claim_a.contains(kw));
        let b_has_neg = NEGATION_INDICATORS.iter().any(|&kw| claim_b.contains(kw));

        if a_has_neg == b_has_neg {
            return false;
        }

        let (negated, positive) = if a_has_neg { (claim_a, claim_b) } else { (claim_b, claim_a) };
        let stripped = self.strip_negation(negated);
        self.similarity(&stripped, positive) > 0.7
    }

    fn is_value_conflict(&self, claim_a: &str, claim_b: &str) -> bool {
        let a_has_uses = claim_a.contains(" uses ") || claim_a.contains(" is ");
        let b_has_uses = claim_b.contains(" uses ") || claim_b.contains(" is ");

        if !a_has_uses || !b_has_uses {
            return false;
        }

        let a_subject = self.extract_subject(claim_a);
        let b_subject = self.extract_subject(claim_b);
        let a_value = self.extract_value(claim_a);
        let b_value = self.extract_value(claim_b);

        if a_subject.is_empty() || b_subject.is_empty() {
            return false;
        }
        if self.similarity(&a_subject, &b_subject) < 0.8 {
            return false;
        }
        if a_value.is_empty() || b_value.is_empty() {
            return false;
        }
        if a_value == b_value {
            return false;
        }

        let a_first = a_value.split_whitespace().next().unwrap_or("");
        let b_first = b_value.split_whitespace().next().unwrap_or("");
        a_first != b_first
    }

    fn is_temporal_contradiction(&self, claim_a: &str, claim_b: &str) -> bool {
        let positive_states = ["recommended", " preferred ", " best practice", " should use", " must use"];
        let negative_states = ["deprecated", " replaced by", " obsolete", " discouraged", " avoid", " anti-pattern"];

        let a_pos = positive_states.iter().any(|&kw| claim_a.contains(kw));
        let a_neg = negative_states.iter().any(|&kw| claim_a.contains(kw));
        let b_pos = positive_states.iter().any(|&kw| claim_b.contains(kw));
        let b_neg = negative_states.iter().any(|&kw| claim_b.contains(kw));

        let opposite = (a_pos && b_neg) || (a_neg && b_pos);
        if !opposite {
            return false;
        }

        let a_subject = self.extract_subject(claim_a);
        let b_subject = self.extract_subject(claim_b);

        if a_subject.is_empty() || b_subject.is_empty() {
            return false;
        }

        self.similarity(&a_subject, &b_subject) > 0.7
    }

    fn strip_negation(&self, claim: &str) -> String {
        let mut result = claim.to_string();
        for neg in NEGATION_INDICATORS {
            result = result.replace(neg, " ");
        }
        self.normalize(&result)
    }

    fn extract_subject(&self, claim: &str) -> String {
        for verb in &[" uses ", " is ", " are ", " was ", " should ", " must ", " can "] {
            if let Some(pos) = claim.find(verb) {
                return claim[..pos].trim().to_string();
            }
        }
        claim.trim().to_string()
    }

    fn extract_value(&self, claim: &str) -> String {
        for verb in &[" uses ", " is ", " are ", " was ", " should ", " must ", " can "] {
            if let Some(pos) = claim.find(verb) {
                return claim[pos + verb.len()..].trim().to_string();
            }
        }
        claim.trim().to_string()
    }

    fn similarity(&self, a: &str, b: &str) -> f64 {
        let words_a: HashSet<&str> = a.split_whitespace().collect();
        let words_b: HashSet<&str> = b.split_whitespace().collect();

        if words_a.is_empty() && words_b.is_empty() {
            return 1.0;
        }
        if words_a.is_empty() || words_b.is_empty() {
            return 0.0;
        }

        let intersection = words_a.intersection(&words_b).count() as f64;
        let union = words_a.union(&words_b).count() as f64;

        if union > 0.0 {
            intersection / union
        } else {
            0.0
        }
    }

    fn is_factual_claim(&self, sentence: &str) -> bool {
        let lower = sentence.to_lowercase();
        CLAIM_KEYWORDS.iter().any(|&kw| lower.contains(kw)) && lower.len() > 10
    }

    fn normalize(&self, sentence: &str) -> String {
        let lower = sentence.to_lowercase();
        let collapsed: String = lower.split_whitespace().collect::<Vec<&str>>().join(" ");
        collapsed
            .trim_end_matches(['.', ',', '!', '?'])
            .to_string()
    }

    fn split_sentences(&self, text: &str) -> Vec<String> {
        let mut sentences = Vec::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('-') || line.starts_with('*') {
                continue;
            }

            let mut current = String::new();
            for ch in line.chars() {
                current.push(ch);
                if matches!(ch, '.' | '!' | '?') {
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() && trimmed.len() > 3 {
                        sentences.push(trimmed);
                    }
                    current = String::new();
                }
            }
            let remaining = current.trim().to_string();
            if !remaining.is_empty() && remaining.len() > 3 {
                sentences.push(remaining);
            }
        }

        sentences
    }

    fn write_contradictions(&self, contradictions: &[DetectedContradiction]) -> Result<()> {
        if contradictions.is_empty() {
            return Ok(());
        }

        let reports_dir = self.wiki_dir.join("reports");
        std::fs::create_dir_all(&reports_dir)?;

        let log_path = reports_dir.join("contradictions.md");
        let mut content = String::from("# Contradictions Detected\n\n");

        for c in contradictions {
            content.push_str(&format!(
                "- `\"{}\" (from {})` vs `\"{}\" (from {})` [{}]\n",
                c.claim_a, c.source_a, c.claim_b, c.source_b, c.contradiction_type,
            ));
        }

        std::fs::write(&log_path, content)?;
        Ok(())
    }
}

impl Default for ContradictionDetectorSkill {
    fn default() -> Self {
        Self {
            wiki_dir: PathBuf::from("."),
        }
    }
}

#[async_trait]
impl Skill for ContradictionDetectorSkill {
    fn id(&self) -> &str {
        "zen-maintenance-contradiction-detector"
    }

    fn description(&self) -> &str {
        "Detect conflicting claims across notes using heuristic pattern matching"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        ctx.evidence.iter().any(|ev| ev.detail.get("notes").is_some())
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

        let notes_array = match notes_val {
            Some(ref v) => v
                .as_array()
                .ok_or_else(|| KernelError::SkillFailed("expected notes array".into()))?
                .clone(),
            None => {
                info!("ContradictionDetectorSkill: no notes in context, skipping");
                return Ok(SkillOutcome::noop());
            }
        };

        let contradictions = self
            .detect(&notes_array)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        let contradiction_count = contradictions.len();

        ctx.evidence.push(rig_compose::context::Evidence::new(
            self.id(),
            "detected_contradictions",
        ).with_detail(serde_json::json!({
            "contradictions": contradictions,
            "count": contradiction_count,
        })));

        if contradiction_count > 0 {
            ctx.signals.push(rig_compose::context::Signal::new("contradictions_found"));
        }

        info!(
            contradiction_count,
            "ContradictionDetectorSkill: execution complete"
        );

        Ok(SkillOutcome::noop().with_delta(
            if contradiction_count > 0 { -0.1 } else { 0.0 },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_negation_contradiction() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        let notes = vec![
            serde_json::json!({"id": "note-1", "content": "Rust is memory safe. It has no garbage collector."}),
            serde_json::json!({"id": "note-2", "content": "Rust is not memory safe for all cases."}),
        ];

        let contradictions = skill.detect(&notes).unwrap();
        assert!(!contradictions.is_empty());
    }

    #[test]
    fn test_detect_value_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        let notes = vec![
            serde_json::json!({"id": "note-1", "content": "This project uses SQLite for storage."}),
            serde_json::json!({"id": "note-2", "content": "This project uses PostgreSQL for the database."}),
        ];

        let contradictions = skill.detect(&notes).unwrap();
        assert!(!contradictions.is_empty());
    }

    #[test]
    fn test_no_contradiction_same_source() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        let notes = vec![
            serde_json::json!({"id": "note-1", "content": "Rust is memory safe. Rust is not memory safe for unsafe code."}),
        ];

        let contradictions = skill.detect(&notes).unwrap();
        assert!(contradictions.is_empty());
    }

    #[test]
    fn test_is_factual_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        assert!(skill.is_factual_claim("Rust is a systems programming language"));
        assert!(!skill.is_factual_claim("hello"));
    }

    #[test]
    fn test_similarity_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        let score = skill.similarity("rust is great", "rust is great");
        assert!((score - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_similarity_no_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let skill = ContradictionDetectorSkill::new(tmp.path().to_path_buf());
        let score = skill.similarity("rust programming", "cooking recipes");
        assert!(score < 0.1);
    }
}
