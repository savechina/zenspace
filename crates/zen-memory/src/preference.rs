//! Preference triples (M4, FR-021 Pi point 2).
//!
//! `SessionJournaler.extract_signals` derives `{subject, predicate, object}`
//! triples from session conversation; this module owns the domain type, the
//! M4 markdown rendering for `wiki/wisdom/preferences/*.md`, and the pure
//! trigger helper consumed by the (separately built) SkillHitRouter (FR-037).
//! Persistence goes through `zen_vault::wiki::AtomicWikiWriter` at the
//! call site — zen-memory deliberately stays IO-of-vault agnostic.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bayesian confidence prior (FR-025): unconfirmed observations start at 0.5.
pub const PREFERENCE_PRIOR: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreferencePredicate {
    Likes,
    Prefers,
}

impl PreferencePredicate {
    pub fn parse_predicate(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "likes" | "like" | "喜欢" => Some(Self::Likes),
            "prefers" | "prefer" | "偏好" => Some(Self::Prefers),
            _ => None,
        }
    }
}

impl std::fmt::Display for PreferencePredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Likes => write!(f, "likes"),
            Self::Prefers => write!(f, "prefers"),
        }
    }
}

/// A preference triple `{subject, predicate, object, confidence, last_seen}`
/// (data-model.md §Preference). Dedup is path-determinism: the same
/// subject+predicate+object always renders to the same file id, so repeated
/// observations refresh `confidence`/`last_seen` instead of duplicating pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub subject: String,
    pub predicate: PreferencePredicate,
    pub object: String,
    pub confidence: f64,
    pub last_seen: DateTime<Utc>,
}

impl Preference {
    pub fn new(
        subject: impl Into<String>,
        predicate: PreferencePredicate,
        object: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate,
            object: object.into(),
            confidence: PREFERENCE_PRIOR,
            last_seen: Utc::now(),
        }
    }

    /// Deterministic M4 page id: `{subject}-{predicate}-{object}` slug.
    pub fn id(&self) -> String {
        slugify(&format!(
            "{}-{}-{}",
            self.subject, self.predicate, self.object
        ))
    }

    /// Confidence clamped to the FR-025 Bayesian band (never 0 or 1).
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.01, 0.99);
        self
    }

    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id()));
        md.push_str("type: preference\n");
        md.push_str(&format!("subject: {}\n", self.subject));
        md.push_str(&format!("predicate: {}\n", self.predicate));
        md.push_str(&format!("object: {}\n", self.object));
        md.push_str(&format!("confidence: {:.2}\n", self.confidence));
        md.push_str(&format!("last_seen: {}\n", self.last_seen.to_rfc3339()));
        md.push_str("---\n\n");
        md.push_str(&format!(
            "# Preference: {} {} {}\n\n",
            self.subject, self.predicate, self.object
        ));
        md.push_str(&format!(
            "- subject: {}\n- predicate: {}\n- object: {}\n",
            self.subject, self.predicate, self.object
        ));
        md
    }

    /// Minimal frontmatter parse for a preference page (subject/predicate/object).
    ///
    /// Canonical parser for `wiki/wisdom/preferences/*.md`: both the DreamWorker
    /// precipitation step and `zen skill run` previously carried byte-identical
    /// copies of this logic (hoisted 2026-09-04). Missing predicate defaults to
    /// [`PreferencePredicate::Likes`]; returns `None` when the page has no
    /// `---` frontmatter block or lacks subject/object.
    pub fn parse_page(content: &str) -> Option<Self> {
        let trimmed = content.trim().strip_prefix("---")?;
        let fm = trimmed.split("\n---").next()?;
        let mut subject = None;
        let mut predicate = None;
        let mut object = None;
        for line in fm.lines() {
            let line = line.trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "subject" => subject = Some(value),
                "predicate" => predicate = PreferencePredicate::parse_predicate(&value),
                "object" => object = Some(value),
                _ => {}
            }
        }
        Some(Self::new(
            subject?,
            predicate.unwrap_or(PreferencePredicate::Likes),
            object?,
        ))
    }
}

/// Pure trigger contribution for the SkillHitRouter (FR-037 Pi hit / FR-021
/// point 5). Returns normalized, order-preserving deduped candidates: the
/// full triple phrase, the bare object, and the subject+object pair. Deliberately
/// IO-free so the router consumes it without coupling to persistence.
pub fn preference_triggers(p: &Preference) -> Vec<String> {
    let candidates = [
        format!("{} {} {}", p.subject, p.predicate, p.object),
        p.object.clone(),
        format!("{} {}", p.subject, p.object),
    ];
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .map(|c| c.trim().to_lowercase())
        .filter(|c| !c.is_empty())
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(80)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_roundtrip() {
        assert_eq!(
            PreferencePredicate::parse_predicate("likes"),
            Some(PreferencePredicate::Likes)
        );
        assert_eq!(
            PreferencePredicate::parse_predicate("prefers"),
            Some(PreferencePredicate::Prefers)
        );
        assert_eq!(
            PreferencePredicate::parse_predicate("喜欢"),
            Some(PreferencePredicate::Likes)
        );
        assert_eq!(PreferencePredicate::parse_predicate("hates"), None);
        assert_eq!(PreferencePredicate::Likes.to_string(), "likes");
        assert_eq!(PreferencePredicate::Prefers.to_string(), "prefers");
    }

    #[test]
    fn id_is_deterministic_per_triple() {
        let a = Preference::new("user", PreferencePredicate::Likes, "concise code");
        let b = Preference::new("user", PreferencePredicate::Likes, "concise code");
        assert_eq!(a.id(), b.id());
        assert_eq!(a.id(), "user-likes-concise-code");
    }

    #[test]
    fn confidence_clamped_to_bayesian_band() {
        let p = Preference::new("user", PreferencePredicate::Likes, "rust").with_confidence(1.5);
        assert_eq!(p.confidence, 0.99);
        let p = p.with_confidence(-1.0);
        assert_eq!(p.confidence, 0.01);
        let p = p.with_confidence(0.7);
        assert_eq!(p.confidence, 0.7);
    }

    #[test]
    fn default_confidence_is_fr025_prior() {
        let p = Preference::new("user", PreferencePredicate::Prefers, "vim");
        assert_eq!(p.confidence, PREFERENCE_PRIOR);
    }

    #[test]
    fn markdown_roundtrip_fields() {
        let mut p = Preference::new("user", PreferencePredicate::Likes, "concise-code");
        p.confidence = 0.8;
        let md = p.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("type: preference"));
        assert!(md.contains("subject: user"));
        assert!(md.contains("predicate: likes"));
        assert!(md.contains("object: concise-code"));
        assert!(md.contains("confidence: 0.80"));
        assert!(md.contains("last_seen: "));
        assert!(md.contains("# Preference: user likes concise-code"));
    }

    #[test]
    fn triggers_full_triple_object_and_pair() {
        let p = Preference::new("user", PreferencePredicate::Likes, "Rust");
        let triggers = preference_triggers(&p);
        assert_eq!(
            triggers,
            vec![
                "user likes rust".to_string(),
                "rust".to_string(),
                "user rust".to_string()
            ]
        );
    }

    #[test]
    fn triggers_dedupe_and_trim() {
        let p = Preference::new("user", PreferencePredicate::Likes, "user");
        let triggers = preference_triggers(&p);
        let unique = triggers.iter().collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), triggers.len(), "no duplicates: {triggers:?}");
        assert!(
            triggers
                .iter()
                .all(|t| !t.starts_with(' ') && !t.ends_with(' '))
        );
    }

    #[test]
    fn parse_page_roundtrips_rendered_frontmatter() {
        let mut p = Preference::new("user", PreferencePredicate::Prefers, "concise code");
        p.confidence = 0.8;
        let parsed = Preference::parse_page(&p.to_markdown()).unwrap();
        assert_eq!(parsed.subject, "user");
        assert_eq!(parsed.predicate, PreferencePredicate::Prefers);
        assert_eq!(parsed.object, "concise code");
    }

    #[test]
    fn parse_page_defaults_predicate_and_rejects_incomplete() {
        let md = "---\ntype: preference\nsubject: user\nobject: vim\n---\n\nbody";
        let parsed = Preference::parse_page(md).unwrap();
        assert_eq!(parsed.predicate, PreferencePredicate::Likes);
        assert!(Preference::parse_page("---\nsubject: user\n---\n").is_none());
        assert!(Preference::parse_page("no frontmatter").is_none());
    }

    #[test]
    fn slugify_strips_punctuation_and_collapses_dashes() {
        assert_eq!(
            slugify("Rust -- systems! language"),
            "rust-systems-language"
        );
        assert_eq!(slugify("  "), "");
        assert_eq!(slugify("简洁代码"), "简洁代码");
    }
}
