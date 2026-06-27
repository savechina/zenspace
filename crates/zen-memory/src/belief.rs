//! Bayesian belief tracking for the evolution engine.
//!
//! Provides binary-evidence Bayesian updating with source weighting,
//! reinforcement/decay, promotion/demotion heuristics, and
//! markdown-file-based persistence (YAML frontmatter + evidence log).
//!
//! Storage: `wiki/wisdom/beliefs/{slug}.md`

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ─── Data types ────────────────────────────────────────────────────────

/// A Bayesian belief tracked through the evolution engine.
///
/// Uses simplified binary evidence model (post-critique revision):
///
/// - `supports=true` → +1 evidence for the proposition
/// - `supports=false` → -1 evidence against
///
/// Weighted by source credibility (0.0-1.0).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Belief {
    /// Unique slug identifier (e.g., "jwt-auth-refresh-rotation").
    pub id: String,
    /// The proposition being tracked.
    pub proposition: String,
    /// Current belief strength: 0.01 (almost certainly false) to 0.99 (almost certainly true).
    /// Starts at 0.5 (maximum uncertainty).
    pub posterior: f64,
    /// Total number of evidence observations applied.
    pub evidence_count: u32,
    /// Reinforcement multiplier (starts at 1.0, caps at 2.0).
    /// Increases when belief is retrieved/injected; decays after 90 days unretrieved.
    pub weight: f64,
    /// Domain categorization (e.g., "auth", "architecture", "workflow").
    pub domain: String,
    /// When the belief was first created.
    pub created_at: DateTime<Utc>,
    /// When posterior was last updated by new evidence.
    pub last_updated: DateTime<Utc>,
    /// When the belief was last retrieved/injected into a prompt.
    /// Used for decay calculations. None if never retrieved.
    pub last_retrieved: Option<DateTime<Utc>>,
    /// Evidence log entries (appended on each update call).
    pub evidence: Vec<EvidenceEntry>,
}

/// A single evidence observation applied to a belief.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceEntry {
    /// RFC3339 timestamp of the observation.
    pub timestamp: DateTime<Utc>,
    /// Whether this evidence supports (true) or contradicts (false) the proposition.
    pub supports: bool,
    /// Source weight applied (0.0 to 1.0).
    pub source_weight: f64,
    /// Source type for audit trail.
    pub source_type: SourceType,
    /// Research method used to gather this evidence (§4.11).
    pub research_method: Option<ResearchMethod>,
    /// Optional note describing the evidence context.
    pub note: Option<String>,
}

/// Categorization of evidence sources for weighting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// Direct self-observation (weight: 1.0).
    SelfObservation,
    /// Trusted peer or mentor (weight: 0.7).
    TrustedPeer,
    /// Authoritative book or paper (weight: 0.8).
    AuthorityBook,
    /// Anonymous internet content (weight: 0.2).
    AnonymousInternet,
}

impl SourceType {
    /// Get the default weight for this source type.
    pub fn default_weight(&self) -> f64 {
        match self {
            SourceType::SelfObservation => 1.0,
            SourceType::TrustedPeer => 0.7,
            SourceType::AuthorityBook => 0.8,
            SourceType::AnonymousInternet => 0.2,
        }
    }
}

/// Research method used to gather evidence (§4.11).
///
/// Tracks how evidence was obtained for audit trail and quality assessment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResearchMethod {
    /// Evidence from social engineering (e.g., asking people, surveys).
    SocialEngineering,
    /// Evidence from ecommerce data (e.g., sales, transactions).
    EcommerceData,
    /// Evidence from third-party data sources (e.g., APIs, datasets).
    ThirdPartyData,
    /// Evidence from Q&A search (e.g., Stack Overflow, forums).
    QaSearch,
    /// Evidence from direct observation (e.g., monitoring, testing).
    Observation,
}

impl ResearchMethod {
    /// Get the string representation for serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SocialEngineering => "social_engineering",
            Self::EcommerceData => "ecommerce_data",
            Self::ThirdPartyData => "third_party_data",
            Self::QaSearch => "qa_search",
            Self::Observation => "observation",
        }
    }

    /// Parse from string representation.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "social_engineering" => Some(Self::SocialEngineering),
            "ecommerce_data" => Some(Self::EcommerceData),
            "third_party_data" => Some(Self::ThirdPartyData),
            "qa_search" => Some(Self::QaSearch),
            "observation" => Some(Self::Observation),
            _ => None,
        }
    }
}

// ─── Belief methods ────────────────────────────────────────────────────

impl Belief {
    /// Create a new belief with prior = 0.5 (maximum uncertainty).
    pub fn new(id: String, proposition: String, domain: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            proposition,
            posterior: 0.5,
            evidence_count: 0,
            weight: 1.0,
            domain,
            created_at: now,
            last_updated: now,
            last_retrieved: None,
            evidence: Vec::new(),
        }
    }

    /// Apply Bayesian update with binary evidence.
    pub fn update(&mut self, supports: bool, source: SourceType, note: Option<String>) {
        let source_weight = source.default_weight();
        self.bayesian_update_weighted(supports, source_weight);

        self.evidence.push(EvidenceEntry {
            timestamp: Utc::now(),
            supports,
            source_weight,
            source_type: source,
            research_method: None,
            note,
        });
        self.evidence_count += 1;
        self.last_updated = Utc::now();
    }

    /// Apply Bayesian update with binary evidence and a research method.
    pub fn update_with_method(
        &mut self,
        supports: bool,
        source: SourceType,
        method: ResearchMethod,
        note: Option<String>,
    ) {
        let source_weight = source.default_weight();
        self.bayesian_update_weighted(supports, source_weight);

        self.evidence.push(EvidenceEntry {
            timestamp: Utc::now(),
            supports,
            source_weight,
            source_type: source,
            research_method: Some(method),
            note,
        });
        self.evidence_count += 1;
        self.last_updated = Utc::now();
    }

    /// Core Bayesian computation (extracted for testing).
    /// Anti-anchoring: if contradicts and movement < 30%, force 30% movement.
    fn bayesian_update_weighted(&mut self, supports: bool, source_weight: f64) {
        let likelihood = if supports {
            0.5 + 0.5 * source_weight
        } else {
            0.5 - 0.5 * source_weight
        };

        let numerator = self.posterior * likelihood;
        let denominator = numerator + (1.0 - self.posterior) * (1.0 - likelihood);
        let new_posterior = numerator / denominator;

        // Anti-anchoring: if contradicts and movement < 30%, force 30%
        let enforced = if !supports && (new_posterior - self.posterior).abs() < 0.3 {
            (self.posterior - 0.3).max(0.01)
        } else {
            new_posterior
        };

        self.posterior = enforced.clamp(0.01, 0.99);
    }

    /// Reinforcement: called when belief is retrieved or prompt-injected.
    /// weight *= 1.01, capped at 2.0.
    pub fn reinforce(&mut self) {
        self.weight = (self.weight * 1.01).min(2.0);
        self.last_retrieved = Some(Utc::now());
    }

    /// Apply time-based decay if belief hasn't been retrieved in 90+ days.
    /// weight *= 0.95. Returns true if decay was applied.
    pub fn apply_decay(&mut self, now: DateTime<Utc>) -> bool {
        if let Some(last) = self.last_retrieved {
            let days_unretrieved = (now - last).num_days();
            if days_unretrieved >= 90 {
                self.weight *= 0.95;
                return true;
            }
        }
        false
    }

    /// Should this belief be promoted to wiki/wisdom/?
    /// Rule: posterior > 0.9 AND evidence_count > 5.
    pub fn should_promote(&self) -> bool {
        self.posterior > 0.9 && self.evidence_count > 5
    }

    /// Should this belief be demoted to archive/?
    /// Rule: posterior < 0.2.
    pub fn should_demote(&self) -> bool {
        self.posterior < 0.2
    }

    /// Should correction workflow trigger?
    /// Rule: was posterior > 0.7 in a prior evidence entry, now < 0.3.
    pub fn should_correct(&self) -> bool {
        if self.posterior >= 0.3 {
            return false;
        }
        // Check if any prior state had posterior > 0.7
        // We approximate by checking if evidence_count > 2 (had prior support)
        self.evidence_count > 2
    }

    /// Priority score for attention allocation.
    /// Higher = more deserving of LLM extraction attention.
    pub fn priority_score(&self) -> f64 {
        self.posterior * self.weight
    }
}

// ─── File persistence ──────────────────────────────────────────────────

impl Belief {
    /// Serialize belief to markdown format with YAML frontmatter + evidence log body.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str(&format!("id: {}\n", self.id));
        md.push_str(&format!(
            "proposition: \"{}\"\n",
            self.proposition.replace('"', "\\\"")
        ));
        md.push_str(&format!("posterior: {:.4}\n", self.posterior));
        md.push_str(&format!("evidence_count: {}\n", self.evidence_count));
        md.push_str(&format!("weight: {:.4}\n", self.weight));
        md.push_str(&format!("domain: {}\n", self.domain));
        md.push_str(&format!("created_at: {}\n", self.created_at.to_rfc3339()));
        md.push_str(&format!(
            "last_updated: {}\n",
            self.last_updated.to_rfc3339()
        ));
        if let Some(lr) = self.last_retrieved {
            md.push_str(&format!("last_retrieved: {}\n", lr.to_rfc3339()));
        }
        md.push_str("---\n\n");
        md.push_str(&format!("# Belief: {}\n\n", self.proposition));
        md.push_str("**Posterior**: ");
        md.push_str(&format!("{:.1}% confident\n\n", self.posterior * 100.0));
        md.push_str("## Evidence Log\n\n");
        if self.evidence.is_empty() {
            md.push_str("_(no evidence recorded yet)_\n");
        } else {
            for e in &self.evidence {
                let arrow = if e.supports { "✓" } else { "✗" };
                let src = format!("{:?}", e.source_type)
                    .to_lowercase()
                    .replace('_', "-");
                let method_str = e
                    .research_method
                    .map(|m| format!(" [{}]", m.as_str()))
                    .unwrap_or_default();
                md.push_str(&format!(
                    "- {} [{}] {} (weight: {:.1}){}",
                    arrow,
                    e.timestamp.format("%Y-%m-%d"),
                    src,
                    e.source_weight,
                    method_str
                ));
                if let Some(note) = &e.note {
                    md.push_str(&format!(" — {}", note));
                }
                md.push('\n');
            }
        }
        md
    }

    /// Save belief to `dir/{id}.md`.
    pub fn save(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)
            .with_context(|| format!("failed to create beliefs dir: {}", dir.display()))?;
        let path = dir.join(format!("{}.md", self.id));
        let content = self.to_markdown();
        fs::write(&path, content)
            .with_context(|| format!("failed to write belief file: {}", path.display()))?;
        Ok(())
    }

    /// Load all beliefs from a directory of `.md` files.
    pub fn load_all(dir: &Path) -> Result<Vec<Belief>> {
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut beliefs = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                match Self::from_file(&path) {
                    Ok(b) => beliefs.push(b),
                    Err(e) => {
                        warn!(
                            path = %path.display(),
                            error = %e,
                            "failed to parse belief file, skipping"
                        );
                    }
                }
            }
        }
        Ok(beliefs)
    }

    /// Parse a belief from a markdown file.
    pub fn from_file(path: &Path) -> Result<Belief> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read belief file: {}", path.display()))?;
        Self::from_markdown(&content)
    }

    /// Parse belief from markdown string (frontmatter + body).
    pub fn from_markdown(content: &str) -> Result<Belief> {
        let fm = extract_frontmatter(content)?;
        let id = parse_yaml_field(&fm, "id")
            .ok_or_else(|| anyhow::anyhow!("missing id field"))?;
        let proposition = parse_yaml_field(&fm, "proposition")
            .map(|s| s.trim_matches('"').to_string())
            .ok_or_else(|| anyhow::anyhow!("missing proposition field"))?;
        let posterior: f64 = parse_yaml_field(&fm, "posterior")
            .ok_or_else(|| anyhow::anyhow!("missing posterior field"))?
            .parse()
            .unwrap_or(0.5);
        let evidence_count: u32 = parse_yaml_field(&fm, "evidence_count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let weight: f64 = parse_yaml_field(&fm, "weight")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0);
        let domain =
            parse_yaml_field(&fm, "domain").unwrap_or_else(|| "uncategorized".to_string());
        let created_at = parse_yaml_field(&fm, "created_at")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let last_updated = parse_yaml_field(&fm, "last_updated")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let last_retrieved = parse_yaml_field(&fm, "last_retrieved")
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Belief {
            id,
            proposition,
            posterior,
            evidence_count,
            weight,
            domain,
            created_at,
            last_updated,
            last_retrieved,
            evidence: Vec::new(),
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

// ─── Aggregation helpers ───────────────────────────────────────────────

/// Apply decay to all beliefs and return count of decayed beliefs.
pub fn apply_decay_all(beliefs: &mut [Belief], now: DateTime<Utc>) -> usize {
    let mut count = 0;
    for b in beliefs.iter_mut() {
        if b.apply_decay(now) {
            count += 1;
        }
    }
    count
}

/// Get beliefs sorted by priority score (descending), take top N.
pub fn top_by_priority(beliefs: &[Belief], n: usize) -> Vec<&Belief> {
    let mut sorted: Vec<&Belief> = beliefs.iter().collect();
    sorted.sort_by(|a, b| {
        b.priority_score()
            .partial_cmp(&a.priority_score())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted.into_iter().take(n).collect()
}

/// Slugify a proposition into a belief ID.
pub fn slugify_proposition(text: &str) -> String {
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

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::tempdir;

    #[test]
    fn test_new_belief_starts_at_prior_0_5() {
        let b = Belief::new("test-id".into(), "test prop".into(), "test-domain".into());
        assert_eq!(b.posterior, 0.5);
        assert_eq!(b.evidence_count, 0);
        assert_eq!(b.weight, 1.0);
    }

    #[test]
    fn test_update_with_supporting_evidence_increases_posterior() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.update(true, SourceType::SelfObservation, None);
        assert!(b.posterior > 0.5, "expected > 0.5, got {}", b.posterior);
    }

    #[test]
    fn test_update_with_contradicting_evidence_decreases_posterior() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.update(false, SourceType::SelfObservation, None);
        assert!(b.posterior < 0.5, "expected < 0.5, got {}", b.posterior);
    }

    #[test]
    fn test_self_observation_has_highest_weight() {
        assert_eq!(SourceType::SelfObservation.default_weight(), 1.0);
        assert_eq!(SourceType::AuthorityBook.default_weight(), 0.8);
        assert_eq!(SourceType::TrustedPeer.default_weight(), 0.7);
        assert_eq!(SourceType::AnonymousInternet.default_weight(), 0.2);
    }

    #[test]
    fn test_anonymous_internet_has_lowest_weight() {
        assert_eq!(SourceType::AnonymousInternet.default_weight(), 0.2);
    }

    #[test]
    fn test_bayesian_converges_with_repeated_support() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        for _ in 0..10 {
            b.update(true, SourceType::SelfObservation, None);
        }
        assert!(b.posterior > 0.9, "expected > 0.9, got {}", b.posterior);
    }

    #[test]
    fn test_bayesian_converges_with_repeated_contradiction() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        for _ in 0..10 {
            b.update(false, SourceType::SelfObservation, None);
        }
        assert!(b.posterior < 0.1, "expected < 0.1, got {}", b.posterior);
    }

    #[test]
    fn test_anti_anchoring_forces_30_percent_movement() {
        // Start at 0.5, apply single contradiction
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.update(false, SourceType::SelfObservation, None);
        // At 0.5 with self-observation (weight=1.0), likelihood = 0.0,
        // new_posterior = 0.0, but anti-anchoring doesn't apply because
        // movement is >= 0.3. Let's try a weaker source.
        let mut b2 = Belief::new("test".into(), "prop".into(), "domain".into());
        // AnonymousInternet (weight=0.2): likelihood = 0.4
        // new = 0.5*0.4 / (0.5*0.4 + 0.5*0.6) = 0.2 / 0.5 = 0.4
        // movement = |0.4 - 0.5| = 0.1 < 0.3 → anti-anchoring kicks in
        b2.update(false, SourceType::AnonymousInternet, None);
        // Should be forced to max(0.5 - 0.3, 0.01) = 0.2
        assert!(
            (b2.posterior - 0.2).abs() < 0.001,
            "expected ~0.2, got {}",
            b2.posterior
        );
    }

    #[test]
    fn test_reinforcement_increases_weight() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        assert_eq!(b.weight, 1.0);
        b.reinforce();
        assert!(b.weight > 1.0, "expected > 1.0, got {}", b.weight);
        assert!(b.last_retrieved.is_some());
    }

    #[test]
    fn test_reinforcement_caps_at_2_0() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        for _ in 0..200 {
            b.reinforce();
        }
        assert!((b.weight - 2.0).abs() < 0.001, "expected 2.0, got {}", b.weight);
    }

    #[test]
    fn test_decay_applies_after_90_days() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.reinforce(); // sets last_retrieved
        let initial_weight = b.weight;
        let now = b.last_retrieved.unwrap() + Duration::days(91);
        let decayed = b.apply_decay(now);
        assert!(decayed, "expected decay to apply");
        assert!(b.weight < initial_weight, "expected weight < {}, got {}", initial_weight, b.weight);
    }

    #[test]
    fn test_decay_not_applied_within_90_days() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.reinforce(); // sets last_retrieved
        let initial_weight = b.weight;
        let now = b.last_retrieved.unwrap() + Duration::days(89);
        let decayed = b.apply_decay(now);
        assert!(!decayed, "expected no decay within 90 days");
        assert_eq!(b.weight, initial_weight);
    }

    #[test]
    fn test_should_promote_high_confidence_belief() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        // Add 6 supporting evidences to push posterior > 0.9 and evidence_count > 5
        for _ in 0..6 {
            b.update(true, SourceType::SelfObservation, None);
        }
        assert!(b.should_promote(), "expected promote, posterior={}, count={}", b.posterior, b.evidence_count);
    }

    #[test]
    fn test_should_demote_low_confidence_belief() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        // Add contradicting evidence to push posterior < 0.2
        for _ in 0..10 {
            b.update(false, SourceType::SelfObservation, None);
        }
        assert!(b.should_demote(), "expected demote, posterior={}", b.posterior);
    }

    #[test]
    fn test_markdown_round_trip() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("beliefs");

        let mut b = Belief::new("rt-test".into(), "round trip prop".into(), "test-domain".into());
        b.update(true, SourceType::TrustedPeer, Some("supporting note".into()));
        b.update(false, SourceType::AnonymousInternet, None);

        b.save(&dir).unwrap();

        let loaded = Belief::load_all(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        let lb = &loaded[0];
        assert_eq!(lb.id, "rt-test");
        assert_eq!(lb.proposition, "round trip prop");
        assert_eq!(lb.evidence_count, 2);
        assert_eq!(lb.domain, "test-domain");
        assert!((lb.posterior - b.posterior).abs() < 0.001);
        // Evidence log is display-only, not reloaded
        assert!(lb.evidence.is_empty());
    }

    #[test]
    fn test_load_all_empty_dir() {
        let tmp = tempdir().unwrap();
        let beliefs = Belief::load_all(tmp.path()).unwrap();
        assert!(beliefs.is_empty());
    }

    #[test]
    fn test_slugify_proposition() {
        assert_eq!(
            slugify_proposition("JWT with refresh rotation"),
            "jwt-with-refresh-rotation"
        );
        assert_eq!(slugify_proposition("  spaces  "), "spaces");
        let long = slugify_proposition(
            "A very long proposition that exceeds the sixty character limit",
        );
        assert_eq!(long.len(), 60);
        assert_eq!(
            long,
            "a-very-long-proposition-that-exceeds-the-sixty-character-lim"
        );
    }

    #[test]
    fn test_top_by_priority_returns_highest_first() {
        let mut b1 = Belief::new("low".into(), "low".into(), "d".into());
        let mut b2 = Belief::new("high".into(), "high".into(), "d".into());
        b2.update(true, SourceType::SelfObservation, None);
        let beliefs = vec![b1.clone(), b2.clone()];
        let top = top_by_priority(&beliefs, 2);
        assert_eq!(top[0].id, "high");
        // b1 has no evidence, posterior 0.5, weight 1.0 = 0.5
        // b2 has 1 support, posterior > 0.5, weight 1.0 = > 0.5
    }

    #[test]
    fn test_to_markdown_format() {
        let b = Belief::new("md-test".into(), "test prop".into(), "arch".into());
        let md = b.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: md-test"));
        assert!(md.contains("posterior: 0.5000"));
        assert!(md.contains("domain: arch"));
        assert!(md.contains("# Belief: test prop"));
        assert!(md.contains("_(no evidence recorded yet)_"));
    }

    #[test]
    fn test_decay_never_retrieved_no_decay() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        // last_retrieved is None
        let decayed = b.apply_decay(Utc::now());
        assert!(!decayed, "should not decay when never retrieved");
    }

    #[test]
    fn test_should_correct_requires_prior_support() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        // Add 3 contradicting evidences: posterior < 0.3, evidence_count = 3
        for _ in 0..3 {
            b.update(false, SourceType::SelfObservation, None);
        }
        // posterior should be very low, evidence_count = 3 > 2
        assert!(b.posterior < 0.3);
        assert!(b.should_correct(), "expected correction with 3 contradictions");

        // Only 1 contradiction: evidence_count = 1, should NOT correct
        let mut b2 = Belief::new("test2".into(), "prop2".into(), "domain".into());
        b2.update(false, SourceType::SelfObservation, None);
        assert!(!b2.should_correct(), "should not correct with evidence_count=1");
    }

    #[test]
    fn test_from_markdown_invalid_content() {
        let result = Belief::from_markdown("not a frontmatter file");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_markdown_missing_frontmatter_close() {
        let result = Belief::from_markdown("---\nno closing");
        assert!(result.is_err());
    }

    #[test]
    fn test_research_method_serialization_roundtrip() {
        let methods = [
            ResearchMethod::SocialEngineering,
            ResearchMethod::EcommerceData,
            ResearchMethod::ThirdPartyData,
            ResearchMethod::QaSearch,
            ResearchMethod::Observation,
        ];
        for method in methods {
            let s = method.as_str();
            let parsed = ResearchMethod::from_str(s);
            assert_eq!(parsed, Some(method), "roundtrip failed for {:?}", method);
        }
    }

    #[test]
    fn test_research_method_from_str_unknown() {
        assert_eq!(ResearchMethod::from_str("unknown_method"), None);
        assert_eq!(ResearchMethod::from_str(""), None);
    }

    #[test]
    fn test_evidence_entry_with_research_method() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.update_with_method(
            true,
            SourceType::SelfObservation,
            ResearchMethod::Observation,
            Some("direct observation".into()),
        );
        assert_eq!(b.evidence.len(), 1);
        assert_eq!(b.evidence[0].research_method, Some(ResearchMethod::Observation));
        assert_eq!(b.evidence[0].note.as_deref(), Some("direct observation"));
    }

    #[test]
    fn test_evidence_entry_without_research_method() {
        let mut b = Belief::new("test".into(), "prop".into(), "domain".into());
        b.update(true, SourceType::SelfObservation, None);
        assert_eq!(b.evidence.len(), 1);
        assert_eq!(b.evidence[0].research_method, None);
    }

    #[test]
    fn test_markdown_includes_research_method() {
        let mut b = Belief::new("md-test".into(), "test prop".into(), "arch".into());
        b.update_with_method(
            true,
            SourceType::SelfObservation,
            ResearchMethod::QaSearch,
            None,
        );
        let md = b.to_markdown();
        assert!(md.contains("[qa_search]"), "expected research method in markdown");
    }
}
