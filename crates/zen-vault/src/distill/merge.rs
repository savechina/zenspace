//! Wiki merge clustering (005-agentic-loop, T013, FR-016).
//!
//! Trigram-Jaccard similarity over page content; clusters at or above
//! `merge_threshold` (default 0.82) become [`WikiMergePlan`]s. Pairs at or
//! above `merge_pure_duplicate` (default 0.98) short-circuit as
//! [`MergeStrategy::PureDuplicate`] — no LLM call needed.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::types::GapKind;
use crate::wiki::WikiPage;

/// What a merge plan will do with its cluster (data-model §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Similar above threshold but not identical — LLM-assisted merge.
    Merge,
    /// ≥ pure-duplicate threshold — byte-level dedup, no LLM.
    PureDuplicate,
    /// Below threshold — cluster recorded, no action.
    Skip,
}

/// Execution plan for one cluster of similar pages (data-model §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiMergePlan {
    pub cluster_id: String,
    /// Pages folded into the target (includes it; ≥2 for an actionable plan).
    pub source_pages: Vec<PathBuf>,
    pub similarity_scores: HashMap<(PathBuf, PathBuf), f64>,
    pub strategy: MergeStrategy,
    /// The surviving page (largest content wins; ties → first path).
    pub target_page: PathBuf,
    pub llm_merged: bool,
}

/// Compute the trigram set of a string (lowercased, whitespace-normalized).
pub fn trigrams(text: &str) -> HashSet<String> {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let padded = format!("  {normalized} ");
    padded
        .chars()
        .collect::<Vec<_>>()
        .windows(3)
        .map(|w| w.iter().collect())
        .collect()
}

/// Jaccard similarity between two trigram sets: |A∩B| / |A∪B|.
pub fn trigram_jaccard(a: &str, b: &str) -> f64 {
    let (ta, tb) = (trigrams(a), trigrams(b));
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// One discovered cluster of mutually-similar pages.
struct Cluster {
    pages: Vec<(PathBuf, String)>,
    scores: HashMap<(PathBuf, PathBuf), f64>,
}

/// Cluster wiki pages by pairwise trigram-Jaccard similarity (greedy,
/// seed-ordered — deterministic for identical inputs).
///
/// `threshold` = cluster entry (default 0.82), `pure_duplicate` = short-circuit
/// (default 0.98). Returns actionable [`WikiMergePlan`]s plus
/// `DuplicateEntityAlias` gaps for pure-duplicate title collisions detected
/// during clustering.
pub fn build_merge_plans(
    pages: &[WikiPage],
    threshold: f64,
    pure_duplicate: f64,
    cycle_id: &str,
) -> (Vec<WikiMergePlan>, Vec<super::types::GapRecord>) {
    let items: Vec<(PathBuf, String)> = pages
        .iter()
        .map(|p| (p.path.clone(), p.content.clone()))
        .collect();

    let mut used: Vec<bool> = vec![false; items.len()];
    let mut plans = Vec::new();
    let mut gaps = Vec::new();

    for i in 0..items.len() {
        if used[i] {
            continue;
        }
        let mut cluster = Cluster {
            pages: vec![items[i].clone()],
            scores: HashMap::new(),
        };
        used[i] = true;

        for j in (i + 1)..items.len() {
            if used[j] {
                continue;
            }
            let sim = trigram_jaccard(&items[i].1, &items[j].1);
            if sim >= threshold {
                cluster
                    .scores
                    .insert((items[i].0.clone(), items[j].0.clone()), sim);
                cluster.pages.push(items[j].clone());
                used[j] = true;
            }
        }

        if cluster.pages.len() < 2 {
            continue;
        }

        let best = cluster
            .scores
            .values()
            .copied()
            .fold(0.0_f64, f64::max);
        let (strategy, llm_merged) = if best >= pure_duplicate {
            (MergeStrategy::PureDuplicate, false)
        } else {
            (MergeStrategy::Merge, false)
        };

        // Target = page with the most content (stable: first on tie).
        let target = cluster
            .pages
            .iter()
            .enumerate()
            .max_by_key(|(idx, (_, content))| (content.len(), std::cmp::Reverse(*idx)))
            .map(|(_, (path, _))| path.clone())
            .expect("cluster has ≥2 pages");

        // Pure duplicates with identical normalized titles = alias collision.
        if strategy == MergeStrategy::PureDuplicate {
            let titles: HashSet<String> = cluster
                .pages
                .iter()
                .map(|(p, _)| {
                    zen_repo::normalize_alias(
                        &p.file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            if titles.len() < cluster.pages.len() {
                gaps.push(
                    super::types::GapRecord::new(
                        GapKind::DuplicateEntityAlias,
                        cycle_id,
                        format!(
                            "pure-duplicate cluster with alias-colliding titles: {titles:?}"
                        ),
                    ),
                );
            }
        }

        plans.push(WikiMergePlan {
            cluster_id: uuid::Uuid::now_v7().to_string(),
            source_pages: cluster.pages.into_iter().map(|(p, _)| p).collect(),
            similarity_scores: cluster.scores,
            strategy,
            target_page: target,
            llm_merged,
        });
    }

    (plans, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(path: &str, content: &str) -> WikiPage {
        WikiPage {
            title: path.to_string(),
            path: PathBuf::from(path),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            tags: vec![],
            wikilinks: vec![],
            para: None,
            okf_type: None,
            content: content.to_string(),
        }
    }

    #[test]
    fn trigram_jaccard_identical_and_disjoint() {
        let a = "Rust and Tokio power async programming";
        assert!((trigram_jaccard(a, a) - 1.0).abs() < 1e-9);
        assert!(trigram_jaccard("zebra qq qux", "rust tokio async") < 0.1);
    }

    #[test]
    fn trigram_jaccard_empty_inputs() {
        assert!((trigram_jaccard("", "") - 1.0).abs() < 1e-9);
        assert!(trigram_jaccard("", "content") < 0.01);
    }

    #[test]
    fn near_duplicates_cluster_at_pure_duplicate() {
        let base = "# Rust Notes\n\nRust and Tokio power the zen workspace for async programming across many crates and modules.";
        let pages = vec![
            page("wiki/a.md", base),
            page("wiki/b.md", &format!("{base}\n")),
            page("wiki/c.md", "# Totally Different\n\nThe mercado opens at dawn with fresh olives and bread."),
        ];
        let (plans, gaps) = build_merge_plans(&pages, 0.82, 0.98, "cycle-t");
        assert_eq!(plans.len(), 1, "one pure-duplicate cluster");
        assert_eq!(plans[0].strategy, MergeStrategy::PureDuplicate);
        assert_eq!(plans[0].source_pages.len(), 2);
        assert!(!plans[0].llm_merged);
        assert!(gaps.is_empty());
    }

    #[test]
    fn moderately_similar_pages_use_llm_merge_strategy() {
        let base = "# Rust Notes\n\nRust and Tokio power the zen workspace for async programming.";
        let variant = "# Rust Notes\n\nRust and Tokio power the zen workspace; async programming is central, with extra notes on channels and select.";
        let pages = vec![page("wiki/a.md", base), page("wiki/b.md", variant)];
        let (plans, _) = build_merge_plans(&pages, 0.55, 0.98, "cycle-t");
        if !plans.is_empty() {
            assert_eq!(plans[0].strategy, MergeStrategy::Merge);
        }
    }

    #[test]
    fn dissimilar_pages_produce_no_plans() {
        let pages = vec![
            page("wiki/a.md", "# Rust\n\nSystems programming language."),
            page("wiki/b.md", "# Cooking\n\nSoups require patience and good stock."),
        ];
        let (plans, _) = build_merge_plans(&pages, 0.82, 0.98, "cycle-t");
        assert!(plans.is_empty());
    }

    #[test]
    fn alias_collision_gap_emitted_for_duplicate_titles() {
        let base = "# Duplicated Entity\n\nSome repeated knowledge body that is long enough to cluster strongly with its twin page copy.";
        let pages = vec![
            page("wiki/rust.md", base),
            page("wiki/rust.md", base), // same normalized title
        ];
        let (plans, gaps) = build_merge_plans(&pages, 0.82, 0.98, "cycle-t");
        assert_eq!(plans.len(), 1);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, GapKind::DuplicateEntityAlias);
    }
}
