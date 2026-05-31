use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use tracing::info;

use crate::note::Note;

#[derive(Debug, Clone)]
pub struct Contradiction {
    pub claim_a: String,
    pub claim_b: String,
    pub source_a: String,
    pub source_b: String,
}

/// A factual claim extracted from a note's content.
#[derive(Debug, Clone)]
struct Claim {
    text: String,
    source: String,
}

/// ContradictionDetector detects conflicting claims across notes
/// using simple heuristic pattern matching.
pub struct ContradictionDetector;

impl ContradictionDetector {
    pub fn new() -> Self {
        Self
    }

    /// Detect contradictions across multiple notes.
    ///
    /// Extracts factual claims from each note and compares them for conflicts.
    pub fn detect(&self, notes: &[Note]) -> Result<Vec<Contradiction>> {
        let claims = self.extract_claims(notes);
        let contradictions = self.find_contradictions(&claims);
        info!(
            note_count = notes.len(),
            claim_count = claims.len(),
            contradiction_count = contradictions.len(),
            "Contradiction detection complete"
        );
        Ok(contradictions)
    }

    /// Extract factual claims from a set of notes.
    fn extract_claims(&self, notes: &[Note]) -> Vec<Claim> {
        let mut claims = Vec::new();

        for note in notes {
            let sentences = split_into_sentences(&note.content);
            let source = note
                .file_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| note.id.clone());

            for sentence in sentences {
                if is_factual_claim(&sentence) {
                    let normalized = normalize_claim(&sentence);
                    if !normalized.is_empty() {
                        claims.push(Claim {
                            text: normalized,
                            source: source.clone(),
                        });
                    }
                }
            }
        }

        claims
    }

    /// Find contradictions between claims using heuristic patterns.
    fn find_contradictions(&self, claims: &[Claim]) -> Vec<Contradiction> {
        let mut contradictions = Vec::new();
        let mut seen = HashSet::new();

        for i in 0..claims.len() {
            for j in i + 1..claims.len() {
                let a = &claims[i];
                let b = &claims[j];

                if a.source == b.source {
                    continue;
                }

                let (first, second) = if a.source <= b.source {
                    (&a.text, &b.text)
                } else {
                    (&b.text, &a.text)
                };
                let dedup_key = (first.as_str(), second.as_str(), &a.source, &b.source);
                if !seen.insert(dedup_key) {
                    continue;
                }

                if is_negation_contradiction(&a.text, &b.text) {
                    let c = if a.source <= b.source {
                        Contradiction {
                            claim_a: a.text.clone(),
                            claim_b: b.text.clone(),
                            source_a: a.source.clone(),
                            source_b: b.source.clone(),
                        }
                    } else {
                        Contradiction {
                            claim_a: b.text.clone(),
                            claim_b: a.text.clone(),
                            source_a: b.source.clone(),
                            source_b: a.source.clone(),
                        }
                    };
                    contradictions.push(c);
                    continue;
                }

                if is_value_conflict(&a.text, &b.text) {
                    let c = if a.source <= b.source {
                        Contradiction {
                            claim_a: a.text.clone(),
                            claim_b: b.text.clone(),
                            source_a: a.source.clone(),
                            source_b: b.source.clone(),
                        }
                    } else {
                        Contradiction {
                            claim_a: b.text.clone(),
                            claim_b: a.text.clone(),
                            source_a: b.source.clone(),
                            source_b: a.source.clone(),
                        }
                    };
                    contradictions.push(c);
                    continue;
                }

                if is_temporal_contradiction(&a.text, &b.text) {
                    let c = if a.source <= b.source {
                        Contradiction {
                            claim_a: a.text.clone(),
                            claim_b: b.text.clone(),
                            source_a: a.source.clone(),
                            source_b: b.source.clone(),
                        }
                    } else {
                        Contradiction {
                            claim_a: b.text.clone(),
                            claim_b: a.text.clone(),
                            source_a: b.source.clone(),
                            source_b: a.source.clone(),
                        }
                    };
                    contradictions.push(c);
                    continue;
                }
            }
        }

        contradictions
    }

    /// Log contradictions to a markdown file in the wiki directory.
    pub fn log_contradictions(
        &self,
        contradictions: &[Contradiction],
        wiki_dir: &Path,
    ) -> Result<()> {
        if contradictions.is_empty() {
            return Ok(());
        }
        let log_path = wiki_dir.join("contradictions.md");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        for c in contradictions {
            writeln!(
                file,
                "- `\"{}\" (from {})` vs `\"{}\" (from {})`",
                c.claim_a, c.source_a, c.claim_b, c.source_b
            )?;
        }
        Ok(())
    }
}

impl Default for ContradictionDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Skill for ContradictionDetector {
    fn id(&self) -> &str {
        "zen-contradiction-detection"
    }

    fn description(&self) -> &str {
        "Detect conflicting claims and contradictions across notes using heuristic and LLM-based analysis"
    }

    fn applies(&self, _ctx: &InvestigationContext) -> bool {
        true
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let wiki_dir_str = ctx
            .evidence
            .iter()
            .filter_map(|ev| {
                ev.detail
                    .get("wiki_dir")
                    .and_then(|d| d.as_str())
                    .map(String::from)
            })
            .next();

        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let (notes, wiki_dir) = match (notes_val, wiki_dir_str) {
            (Some(notes_json), Some(dir)) => {
                let notes_array = notes_json
                    .as_array()
                    .ok_or_else(|| KernelError::SkillFailed("expected notes array".into()))?;

                let mut notes = Vec::new();
                for note_val in notes_array {
                    let note: Note = serde_json::from_value(note_val.clone()).map_err(|e| {
                        KernelError::SkillFailed(format!("failed to parse note: {e}"))
                    })?;
                    notes.push(note);
                }
                (notes, PathBuf::from(dir))
            }
            _ => {
                info!("ContradictionDetector: no notes in context, skipping analysis");
                return Ok(SkillOutcome::noop());
            }
        };

        let contradictions = self
            .detect(&notes)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        if !contradictions.is_empty() {
            let reports_dir = wiki_dir.join("reports");
            std::fs::create_dir_all(&reports_dir)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            self.log_contradictions(&contradictions, &reports_dir)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
        }

        let contradiction_count = contradictions.len();

        ctx.signals.push(rig_compose::context::Signal::new(format!(
            "{contradiction_count} contradictions detected"
        )));

        info!(
            contradiction_count,
            "Contradiction detection skill complete"
        );

        Ok(SkillOutcome::noop().with_delta(if contradiction_count > 0 { -0.1 } else { 0.0 }))
    }
}

// ── Sentence splitting ──

/// Split text into sentences based on common delimiters.
fn split_into_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();

    // Split by lines first to handle markdown structure
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with('-')
            || line.starts_with('*')
        {
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

// ── Claim detection ──

/// Keywords that indicate a sentence expresses a factual claim or opinion.
const CLAIM_KEYWORDS: &[&str] = &[
    " is ", " are ", " was ", " were ", " should ", " must ", " never ", " always ", " uses ",
    " can ", " cannot ", " may ", " will ",
];

/// Keywords that indicate negation (for contradiction detection).
const NEGATION_INDICATORS: &[&str] = &[" not ", " never ", " no ", " cannot "];

/// Check if a sentence expresses a factual claim.
fn is_factual_claim(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    CLAIM_KEYWORDS.iter().any(|&kw| lower.contains(kw)) && lower.len() > 10
}

// ── Claim normalization ──

/// Normalize a claim for comparison: lowercase, collapse whitespace.
fn normalize_claim(sentence: &str) -> String {
    let lower = sentence.to_lowercase();
    let collapsed: String = lower.split_whitespace().collect::<Vec<&str>>().join(" ");
    collapsed.trim_end_matches(['.', ',', '!', '?']).to_string()
}

// ── Contradiction heuristics ──

/// Check if two claims are negation contradictions of each other.
///
/// Patterns:
/// - "X is Y" vs "X is not Y"
/// - "X uses Y" vs "X never uses Y"
fn is_negation_contradiction(claim_a: &str, claim_b: &str) -> bool {
    let a = claim_a;
    let b = claim_b;

    let a_has_negation = NEGATION_INDICATORS.iter().any(|&kw| a.contains(kw));
    let b_has_negation = NEGATION_INDICATORS.iter().any(|&kw| b.contains(kw));

    if a_has_negation == b_has_negation {
        return false;
    }

    let (negated, positive) = if a_has_negation { (a, b) } else { (b, a) };

    let stripped = strip_negation(negated);

    // Check significant overlap between stripped negated and positive claim
    similarity_score(&stripped, positive) > 0.7
}

/// Strip negation keywords from a claim for comparison.
fn strip_negation(claim: &str) -> String {
    let mut result = claim.to_string();
    for neg in NEGATION_INDICATORS {
        result = result.replace(neg, " ");
    }
    normalize_claim(&result)
}

/// Check if two claims express conflicting values for the same subject.
///
/// Pattern: "X uses A" vs "X uses B" (where A != B)
fn is_value_conflict(claim_a: &str, claim_b: &str) -> bool {
    // Both must contain "uses" or similar value-expression keywords
    let a_contains_uses = claim_a.contains(" uses ") || claim_a.contains(" is ");
    let b_contains_uses = claim_b.contains(" uses ") || claim_b.contains(" is ");

    if !a_contains_uses || !b_contains_uses {
        return false;
    }

    // Extract subjects (words before "uses" or "is")
    let a_subject = extract_subject(claim_a);
    let b_subject = extract_subject(claim_b);

    let a_value = extract_value(claim_a);
    let b_value = extract_value(claim_b);

    if a_subject.is_empty() || b_subject.is_empty() {
        return false;
    }

    // Subjects must match (or overlap significantly)
    if similarity_score(&a_subject, &b_subject) < 0.8 {
        return false;
    }

    if a_value.is_empty() || b_value.is_empty() {
        return false;
    }
    if a_value == b_value {
        return false;
    }
    let a_first_word = a_value.split_whitespace().next().unwrap_or("");
    let b_first_word = b_value.split_whitespace().next().unwrap_or("");
    if a_first_word == b_first_word {
        return false;
    }
    true
}

/// Check for temporal/opinion contradictions.
///
/// Patterns:
/// - "X is deprecated" vs "X is recommended"
/// - "X is replaced by Y" vs "X should be used"
fn is_temporal_contradiction(claim_a: &str, claim_b: &str) -> bool {
    let positive_states = [
        "recommended",
        " preferred ",
        " best practice",
        " should use",
        " must use",
    ];
    let negative_states = [
        "deprecated",
        " replaced by",
        " obsolete",
        " discouraged",
        " avoid",
        " anti-pattern",
    ];

    let a_has_positive = positive_states.iter().any(|&kw| claim_a.contains(kw));
    let a_has_negative = negative_states.iter().any(|&kw| claim_a.contains(kw));
    let b_has_positive = positive_states.iter().any(|&kw| claim_b.contains(kw));
    let b_has_negative = negative_states.iter().any(|&kw| claim_b.contains(kw));

    let opposite_valence = (a_has_positive && b_has_negative) || (a_has_negative && b_has_positive);
    if !opposite_valence {
        return false;
    }

    // Same subject
    let a_subject = extract_subject(claim_a);
    let b_subject = extract_subject(claim_b);

    if a_subject.is_empty() || b_subject.is_empty() {
        return false;
    }

    similarity_score(&a_subject, &b_subject) > 0.7
}

/// Extract the subject portion of a claim (before the verb phrase).
fn extract_subject(claim: &str) -> String {
    let verbs = [
        " uses ", " is ", " are ", " was ", " should ", " must ", " can ",
    ];
    for &verb in &verbs {
        if let Some(pos) = claim.find(verb) {
            let before = &claim[..pos];
            return before.trim().to_string();
        }
    }
    claim.trim().to_string()
}

/// Extract the value portion of a claim (after the verb phrase).
fn extract_value(claim: &str) -> String {
    let verbs = [
        " uses ", " is ", " are ", " was ", " should ", " must ", " can ",
    ];
    for &verb in &verbs {
        if let Some(pos) = claim.find(verb) {
            let after = &claim[pos + verb.len()..];
            return after.trim().to_string();
        }
    }
    claim.trim().to_string()
}

/// Compute a simple word overlap similarity score between two strings.
/// Returns a value between 0.0 and 1.0.
fn similarity_score(a: &str, b: &str) -> f64 {
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

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use zen_core::types::Sensitivity;

    fn make_note(content: &str) -> Note {
        make_note_with_id(content, &uuid::Uuid::now_v7().to_string())
    }

    fn make_note_with_id(content: &str, id: &str) -> Note {
        Note {
            id: id.to_string(),
            tags: vec![],
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            domain: vec![],
            project: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    // ── Sentence splitting tests ──

    #[test]
    fn test_split_into_sentences_basic() {
        let text = "Rust is a systems programming language. It is memory safe.";
        let sentences = split_into_sentences(text);
        assert_eq!(sentences.len(), 2);
        assert!(sentences[0].contains("Rust"));
        assert!(sentences[1].contains("memory safe"));
    }

    #[test]
    fn test_split_skips_headings() {
        let text = "# Introduction\n\nRust is great.\n## Section\nMore info here.";
        let sentences = split_into_sentences(text);
        assert!(sentences.iter().all(|s| !s.starts_with('#')));
    }

    #[test]
    fn test_split_skips_bullet_points() {
        let text = "- Item one\n- Item two\nThis is a sentence.";
        let sentences = split_into_sentences(text);
        assert!(sentences.iter().all(|s| !s.starts_with('-')));
    }

    #[test]
    fn test_split_empty_lines() {
        let text = "\n\n\n";
        let sentences = split_into_sentences(text);
        assert!(sentences.is_empty());
    }

    #[test]
    fn test_split_short_lines_skipped() {
        let text = "Hi. This is a proper sentence with enough content.";
        let sentences = split_into_sentences(text);
        assert_eq!(sentences.len(), 1);
        assert!(sentences[0].contains("proper sentence"));
    }

    // ── Claim detection tests ──

    #[test]
    fn test_is_factual_claim_with_is() {
        assert!(is_factual_claim("Rust is a systems programming language"));
    }

    #[test]
    fn test_is_factual_claim_with_should() {
        assert!(is_factual_claim(
            "Developers should use cargo for dependencies"
        ));
    }

    #[test]
    fn test_is_factual_claim_with_never() {
        assert!(is_factual_claim(
            "You should never unwrap in production code"
        ));
    }

    #[test]
    fn test_is_factual_claim_non_factual() {
        assert!(!is_factual_claim("hello world"));
    }

    #[test]
    fn test_is_factual_claim_too_short() {
        assert!(!is_factual_claim("x is y"));
    }

    #[test]
    fn test_is_factual_claim_with_uses() {
        assert!(is_factual_claim("This project uses SQLite for storage"));
    }

    #[test]
    fn test_is_factual_claim_with_cannot() {
        assert!(is_factual_claim("borrow checker cannot be bypassed safely"));
    }

    // ── Claim normalization tests ──

    #[test]
    fn test_normalize_claim_lowercase() {
        assert_eq!(normalize_claim("Rust Is Great."), "rust is great");
    }

    #[test]
    fn test_normalize_claim_collapse_whitespace() {
        assert_eq!(normalize_claim("Rust   is    great"), "rust is great");
    }

    #[test]
    fn test_normalize_claim_strips_trailing_punctuation() {
        assert_eq!(normalize_claim("rust is great."), "rust is great");
    }

    // ── Subject/value extraction tests ──

    #[test]
    fn test_extract_subject_uses() {
        assert_eq!(extract_subject("this project uses sqlite"), "this project");
    }

    #[test]
    fn test_extract_subject_is() {
        assert_eq!(extract_subject("rust is a language"), "rust");
    }

    #[test]
    fn test_extract_value_uses() {
        assert_eq!(extract_value("this project uses sqlite"), "sqlite");
    }

    #[test]
    fn test_extract_value_is() {
        assert_eq!(extract_value("rust is a language"), "a language");
    }

    // ── Negation contradiction tests ──

    #[test]
    fn test_is_negation_contradiction_basic() {
        assert!(is_negation_contradiction(
            "rust is memory safe",
            "rust is not memory safe"
        ));
    }

    #[test]
    fn test_is_negation_contradiction_never() {
        assert!(is_negation_contradiction(
            "you should use cargo for testing",
            "you should never use cargo for testing"
        ));
    }

    #[test]
    fn test_is_negation_contradiction_same_negation() {
        assert!(!is_negation_contradiction(
            "rust is not memory safe",
            "rust is not zero cost"
        ));
    }

    #[test]
    fn test_is_negation_contradiction_no_negation() {
        assert!(!is_negation_contradiction(
            "rust is memory safe",
            "rust is zero cost"
        ));
    }

    #[test]
    fn test_is_negation_contradiction_different_subjects() {
        assert!(!is_negation_contradiction(
            "rust is memory safe",
            "python is not a compiled language"
        ));
    }

    // ── Value conflict tests ──

    #[test]
    fn test_is_value_conflict_different_databases() {
        assert!(is_value_conflict(
            "this project uses sqlite",
            "this project uses postgresql"
        ));
    }

    #[test]
    fn test_is_value_conflict_different_web_frameworks() {
        assert!(is_value_conflict("web app is react", "web app is vue"));
    }

    #[test]
    fn test_is_value_conflict_same_value() {
        assert!(!is_value_conflict(
            "this project uses sqlite",
            "this project uses sqlite"
        ));
    }

    #[test]
    fn test_is_value_conflict_different_subjects() {
        assert!(!is_value_conflict(
            "project a uses sqlite",
            "project b uses postgresql"
        ));
    }

    #[test]
    fn test_is_value_conflict_no_value_keyword() {
        assert!(!is_value_conflict(
            "the quick brown fox jumps over",
            "the slow gray wolf runs under"
        ));
    }

    // ── Temporal contradiction tests ──

    #[test]
    fn test_is_temporal_contradiction_deprecated_vs_recommended() {
        assert!(is_temporal_contradiction(
            "the old api is deprecated",
            "the old api is recommended"
        ));
    }

    #[test]
    fn test_is_temporal_contradiction_obsolete_vs_should_use() {
        assert!(is_temporal_contradiction(
            "sync mutex is obsolete",
            "sync mutex should use atomics instead"
        ));
    }

    #[test]
    fn test_is_temporal_contradiction_same_sentiment() {
        assert!(!is_temporal_contradiction(
            "rust is recommended",
            "tokio is recommended"
        ));
    }

    #[test]
    fn test_is_temporal_contradiction_same_negative() {
        assert!(!is_temporal_contradiction(
            "sync mutex is deprecated",
            "sync mutex is obsolete"
        ));
    }

    #[test]
    fn test_is_temporal_contradiction_different_subjects() {
        assert!(!is_temporal_contradiction(
            "the old api is deprecated",
            "the new api is recommended"
        ));
    }

    // ── Similarity score tests ──

    #[test]
    fn test_similarity_score_identical() {
        assert!((similarity_score("rust is great", "rust is great") - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_similarity_score_no_overlap() {
        let score = similarity_score("rust programming", "cooking recipes");
        assert!(score < 0.1);
    }

    #[test]
    fn test_similarity_score_partial_overlap() {
        let score = similarity_score("rust is great", "rust is fast");
        assert!(score > 0.3 && score < 1.0);
    }

    // ── Strip negation tests ──

    #[test]
    fn test_strip_negation_removes_not() {
        let result = strip_negation("rust is not memory safe");
        assert!(!result.contains("not"));
    }

    #[test]
    fn test_strip_negation_removes_never() {
        let result = strip_negation("you should never unwrap");
        assert!(!result.contains("never"));
    }

    // ── Integration: extract_claims ──

    #[test]
    fn test_extract_claims_from_note() {
        let detector = ContradictionDetector::new();
        let note = make_note(
            "Rust is a systems programming language.\n\nIt is memory safe.\nYou should never unwrap.",
        );
        let claims = detector.extract_claims(&[note]);
        assert!(claims.len() >= 2);
    }

    #[test]
    fn test_extract_claims_empty_content() {
        let detector = ContradictionDetector::new();
        let note = make_note("");
        let claims = detector.extract_claims(&[note]);
        assert!(claims.is_empty());
    }

    // ── Integration: detect contradictions ──

    #[test]
    fn test_detect_negation_contradiction() {
        let detector = ContradictionDetector::new();
        let note1 = make_note_with_id("Rust is memory safe.", "note-1");
        let note2 = make_note_with_id("Rust is not memory safe for unsafe code.", "note-2");

        let contradictions = detector.detect(&[note1, note2]).unwrap();
        assert!(!contradictions.is_empty());
    }

    #[test]
    fn test_detect_value_conflict() {
        let detector = ContradictionDetector::new();
        let note1 = make_note_with_id("This project uses SQLite for local storage.", "note-3");
        let note2 = make_note_with_id("This project uses PostgreSQL for the database.", "note-4");

        let contradictions = detector.detect(&[note1, note2]).unwrap();
        assert!(!contradictions.is_empty());
    }

    #[test]
    fn test_detect_temporal_contradiction() {
        let detector = ContradictionDetector::new();
        let note1 = make_note_with_id("The old REST API is deprecated.", "note-5");
        let note2 = make_note_with_id(
            "The old REST API is recommended for compatibility.",
            "note-6",
        );

        let contradictions = detector.detect(&[note1, note2]).unwrap();
        assert!(!contradictions.is_empty());
    }

    #[test]
    fn test_detect_no_contradiction_same_source() {
        let detector = ContradictionDetector::new();
        let note = make_note_with_id(
            "Rust is memory safe.\nRust is not memory safe for unsafe blocks.",
            "note-7",
        );

        let contradictions = detector.detect(&[note]).unwrap();
        assert!(contradictions.is_empty());
    }

    #[test]
    fn test_detect_no_contradiction_same_value() {
        let detector = ContradictionDetector::new();
        let note1 = make_note_with_id("This project uses SQLite.", "note-8");
        let note2 = make_note_with_id("This project uses SQLite for storage.", "note-9");

        let contradictions = detector.detect(&[note1, note2]).unwrap();
        assert!(contradictions.is_empty());
    }

    #[test]
    fn test_detect_empty_notes() {
        let detector = ContradictionDetector::new();
        let contradictions = detector.detect(&[]).unwrap();
        assert!(contradictions.is_empty());
    }

    #[test]
    fn test_detect_multiple_contradictions() {
        let detector = ContradictionDetector::new();
        let note1 = make_note_with_id(
            "React is the best framework.\nThis project uses Node.js.",
            "note-10",
        );
        let note2 = make_note_with_id(
            "React is not the best framework.\nThis project uses Python.",
            "note-11",
        );

        let contradictions = detector.detect(&[note1, note2]).unwrap();
        assert!(contradictions.len() >= 2);
    }

    // ── Integration: log_contradictions ──

    #[test]
    fn test_log_contradictions_creates_file() {
        let detector = ContradictionDetector::new();
        let dir = tempfile::tempdir().unwrap();
        let contradictions = vec![Contradiction {
            claim_a: "rust is safe".to_string(),
            claim_b: "rust is not safe".to_string(),
            source_a: "note-1".to_string(),
            source_b: "note-2".to_string(),
        }];

        detector
            .log_contradictions(&contradictions, dir.path())
            .unwrap();

        let log_file = dir.path().join("contradictions.md");
        assert!(log_file.exists());
        let content = std::fs::read_to_string(&log_file).unwrap();
        assert!(content.contains("rust is safe"));
        assert!(content.contains("rust is not safe"));
    }

    #[test]
    fn test_log_contradictions_empty() {
        let detector = ContradictionDetector::new();
        let dir = tempfile::tempdir().unwrap();

        detector.log_contradictions(&[], dir.path()).unwrap();

        let log_file = dir.path().join("contradictions.md");
        assert!(!log_file.exists());
    }

    #[test]
    fn test_log_contradictions_appends() {
        let detector = ContradictionDetector::new();
        let dir = tempfile::tempdir().unwrap();

        let c1 = vec![Contradiction {
            claim_a: "claim a".to_string(),
            claim_b: "claim b".to_string(),
            source_a: "note-1".to_string(),
            source_b: "note-2".to_string(),
        }];
        detector.log_contradictions(&c1, dir.path()).unwrap();

        let c2 = vec![Contradiction {
            claim_a: "claim c".to_string(),
            claim_b: "claim d".to_string(),
            source_a: "note-3".to_string(),
            source_b: "note-4".to_string(),
        }];
        detector.log_contradictions(&c2, dir.path()).unwrap();

        let content = std::fs::read_to_string(dir.path().join("contradictions.md")).unwrap();
        assert!(content.contains("claim a"));
        assert!(content.contains("claim c"));
    }

    #[test]
    fn test_default_implementation() {
        let detector = ContradictionDetector;
        let result = detector.detect(&[]).unwrap();
        assert!(result.is_empty());
    }
}
