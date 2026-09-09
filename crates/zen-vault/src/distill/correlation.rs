//! Gap correlation for the Discover Loop (PD-04, G1).
//!
//! [`correlate`] clusters [`GapRecord`]s that share a normalized entity
//! identity into ranked [`Opportunity`]s, so the discover loop works on
//! improvement opportunities instead of isolated gap rows.
//!
//! Scoring reuses [`confidence_for`] (hypothesis.rs) as the base signal:
//! machine-verified structural gaps outrank possibly-benign ones, and
//! multi-gap clusters get a small multiplicity bonus (capped at 1.0).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::hypothesis::confidence_for;
use super::merge::normalize_notion_name;
use super::types::GapRecord;

/// One cluster of related gaps — the unit the discover loop acts on.
///
/// A cluster groups every gap that refers to the same normalized entity
/// (or, failing that, the same source path). Gaps with neither an entity
/// nor a path each form a singleton opportunity so unrelated gaps never
/// merge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    /// Deterministic id: `opp-{slugified key}-{hash8 of member ids}`.
    /// Stable across runs for identical input.
    pub id: String,
    /// Normalized cluster key (entity name, path stem, or `gap:{id8}`).
    pub entity_key: String,
    /// Member gaps in input order.
    pub member_gaps: Vec<GapRecord>,
    /// Mean member [`confidence_for`] × multiplicity bonus, capped at 1.0.
    pub score: f64,
    /// Human-readable reason (member count, key kind, member kinds).
    pub reason: String,
}

/// Cluster key for one gap: normalized entity, else path stem, else gap id.
///
/// # Arguments
///
/// * `gap` — The gap to key.
///
/// # Returns
///
/// `(key, kind)` where kind is one of `"entity"`, `"path"`, `"id"`.
///
/// # Examples
///
/// ```
/// use zen_vault::distill::correlation::correlate;
/// use zen_vault::distill::types::{GapKind, GapRecord};
///
/// let gaps = vec![
///     GapRecord::new(GapKind::OrphanEntity, "c1", "orphan: Cache").with_entity("Cache"),
/// ];
/// let opps = correlate(&gaps);
/// assert_eq!(opps.len(), 1);
/// assert_eq!(opps[0].member_gaps.len(), 1);
/// ```
fn entity_key_for(gap: &GapRecord) -> (String, &'static str) {
    if let Some(entity) = gap.subject_entity.as_deref() {
        let norm = normalize_notion_name(entity);
        if !norm.is_empty() {
            return (norm, "entity");
        }
    }
    if let Some(path) = gap.subject_path.as_deref() {
        let stem = std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let norm = normalize_notion_name(&stem);
        if !norm.is_empty() {
            return (norm, "path");
        }
    }
    // No entity and no usable path: fall back to the FULL gap id.
    // (uuid v7 prefixes are timestamp-based and collide within the same
    // tick, so a truncated id would wrongly merge unrelated gaps.)
    (format!("gap:{}", gap.id), "id")
}

/// First 8 hex chars of the SHA-256 of `input` (matches slug hash style).
fn short_hash(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Slugify a cluster key for id construction (deterministic, ASCII).
fn slug_key(key: &str) -> String {
    let slug: String = key
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "gap".to_string()
    } else {
        trimmed.chars().take(32).collect()
    }
}

/// Cluster gaps into score-ranked opportunities.
///
/// Groups by normalized entity identity (BTreeMap iteration keeps key
/// order stable), scores each cluster, and sorts by score descending
/// with id ascending as the tiebreak — fully deterministic.
///
/// # Arguments
///
/// * `gaps` — Gap records to cluster (borrowed, never mutated).
///
/// # Returns
///
/// Opportunities sorted by `score` descending. Empty input yields empty
/// output.
pub fn correlate(gaps: &[GapRecord]) -> Vec<Opportunity> {
    let mut clusters: BTreeMap<String, Vec<&GapRecord>> = BTreeMap::new();
    let mut key_kinds: BTreeMap<String, &'static str> = BTreeMap::new();
    for gap in gaps {
        let (key, kind) = entity_key_for(gap);
        clusters.entry(key.clone()).or_default().push(gap);
        key_kinds.entry(key).or_insert(kind);
    }
    let mut out: Vec<Opportunity> = clusters
        .into_iter()
        .map(|(key, members)| {
            let mut kinds: Vec<String> = members
                .iter()
                .map(|g| g.kind.as_str().to_string())
                .collect();
            kinds.sort();
            kinds.dedup();
            let total_conf: f64 = members.iter().map(|g| confidence_for(g.kind)).sum();
            let count = members.len().max(1) as f64;
            let score = (total_conf / count * (1.0 + 0.1 * (count - 1.0))).min(1.0);
            let mut member_ids: Vec<&str> = members.iter().map(|g| g.id.as_str()).collect();
            member_ids.sort_unstable();
            let id = format!(
                "opp-{}-{}",
                slug_key(&key),
                short_hash(&member_ids.join("|"))
            );
            // Full uuid keys are unreadable in reasons — show the short form.
            let display = if key_kinds[&key] == "id" {
                key.chars().take(12).collect::<String>()
            } else {
                key.clone()
            };
            let reason = format!(
                "{} gap(s) share {} '{}': {}",
                members.len(),
                key_kinds[&key],
                display,
                kinds.join(", ")
            );
            Opportunity {
                id,
                entity_key: key,
                member_gaps: members.into_iter().cloned().collect(),
                score,
                reason,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::types::GapKind;

    fn gap(kind: GapKind, entity: &str) -> GapRecord {
        GapRecord::new(kind, "cycle-1", format!("gap: {entity}")).with_entity(entity)
    }

    #[test]
    fn same_entity_across_kinds_forms_one_opportunity() {
        let gaps = vec![
            gap(GapKind::OrphanEntity, "Cache"),
            gap(GapKind::DecisionBlocked, "Cache"),
            gap(GapKind::CommitmentOverdue, "Cache"),
        ];
        let opps = correlate(&gaps);
        assert_eq!(opps.len(), 1);
        assert_eq!(opps[0].member_gaps.len(), 3);
        // avg(0.7, 0.65, 0.6) = 0.65 × 1.2 multiplicity = 0.78
        assert!((opps[0].score - 0.78).abs() < 1e-9);
        // Entity keys are normalized (lowercased) for clustering.
        assert!(opps[0].reason.contains("cache"));
    }

    #[test]
    fn unrelated_entities_stay_separate() {
        let gaps = vec![
            gap(GapKind::OrphanEntity, "Cache"),
            gap(GapKind::OrphanEntity, "Router"),
        ];
        let opps = correlate(&gaps);
        assert_eq!(opps.len(), 2);
        assert!(opps.iter().all(|o| o.member_gaps.len() == 1));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(correlate(&[]).is_empty());
    }

    #[test]
    fn output_is_deterministic_for_shuffled_input() {
        let forward = vec![
            gap(GapKind::OrphanEntity, "Cache"),
            gap(GapKind::DecisionBlocked, "Cache"),
            gap(GapKind::WikiPageWithoutEntities, "Router"),
        ];
        let mut backward = forward.clone();
        backward.reverse();
        let a: Vec<String> = correlate(&forward).iter().map(|o| o.id.clone()).collect();
        let b: Vec<String> = correlate(&backward).iter().map(|o| o.id.clone()).collect();
        assert_eq!(a, b);
        // Higher-confidence cluster (Cache: avg(0.7,0.65)×1.1) ranks first.
        assert!(correlate(&forward)[0].entity_key.contains("cache"));
    }

    #[test]
    fn keyless_gaps_become_singletons() {
        let gaps = vec![
            GapRecord::new(GapKind::LlmFailure, "c1", "llm down"),
            GapRecord::new(GapKind::LlmFailure, "c1", "llm down again"),
        ];
        let opps = correlate(&gaps);
        assert_eq!(opps.len(), 2);
    }
}
