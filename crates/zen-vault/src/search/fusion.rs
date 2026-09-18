use std::collections::HashMap;

use super::SearchResult;

/// RRF constant from the original Cormack et al. formulation, also used by
/// Graphiti/Zep's combined hybrid search (`(k + rank)` with k = 60).
pub const RRF_K: f64 = 60.0;

/// One tier's ranked contribution to a fused search.
#[derive(Debug, Clone)]
pub struct RankedList {
    /// Stable tier label surfaced in provenance (`fts5`, `vec0`, `graph`).
    pub source: &'static str,
    /// Per-tier confidence weight; 1.0 = full trust.
    pub weight: f64,
    /// Tier results, best first (rank 1 = index 0).
    pub results: Vec<SearchResult>,
}

/// A fused hit with its RRF score and the tiers that produced it.
#[derive(Debug, Clone)]
pub struct FusedResult {
    pub result: SearchResult,
    pub score: f64,
    pub provenance: Vec<String>,
}

/// Fuse ranked lists with Reciprocal Rank Fusion:
/// `score(d) = Σ weight_t / (k + rank_t(d))`.
///
/// Documents are identified by `(file, line)` so the same location returned
/// by several tiers accumulates score and provenance instead of appearing
/// multiple times. Ties break on first-seen order (deterministic). The
/// returned list is truncated to `limit` and ordered best-first.
pub fn reciprocal_rank_fusion(lists: &[RankedList], k: f64, limit: usize) -> Vec<FusedResult> {
    let mut order: Vec<(std::path::PathBuf, u32)> = Vec::new();
    let mut acc: HashMap<(std::path::PathBuf, u32), FusedResult> = HashMap::new();

    for list in lists {
        if list.weight == 0.0 {
            continue;
        }
        for (idx, result) in list.results.iter().enumerate() {
            let rank = (idx + 1) as f64;
            let key = (result.file.clone(), result.line);
            let contribution = list.weight / (k + rank);
            match acc.get_mut(&key) {
                Some(existing) => {
                    existing.score += contribution;
                    existing.provenance.push(list.source.to_string());
                }
                None => {
                    order.push(key.clone());
                    acc.insert(
                        key,
                        FusedResult {
                            result: result.clone(),
                            score: contribution,
                            provenance: vec![list.source.to_string()],
                        },
                    );
                }
            }
        }
    }

    let mut fused: Vec<FusedResult> = order
        .into_iter()
        .filter_map(|key| acc.remove(&key))
        .collect();
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fused.truncate(limit);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hit(file: &str, line: u32) -> SearchResult {
        SearchResult {
            file: PathBuf::from(file),
            line,
            content: format!("{file}:{line}"),
        }
    }

    fn list(source: &'static str, weight: f64, results: Vec<SearchResult>) -> RankedList {
        RankedList {
            source,
            weight,
            results,
        }
    }

    #[test]
    fn rrf_scores_follow_rank_and_weight() {
        let fused = reciprocal_rank_fusion(
            &[
                list("fts5", 1.0, vec![hit("a.md", 1), hit("b.md", 1)]),
                list("vec0", 1.0, vec![hit("b.md", 1)]),
            ],
            RRF_K,
            10,
        );
        assert_eq!(fused.len(), 2);
        // b.md appears in both lists (ranks 2 and 1) → fusion promotes it.
        assert_eq!(fused[0].result.file, PathBuf::from("b.md"));
        assert_eq!(fused[0].provenance.len(), 2);
        let expected_b = 1.0 / (RRF_K + 2.0) + 1.0 / (RRF_K + 1.0);
        assert!((fused[0].score - expected_b).abs() < 1e-12);
        assert!((fused[1].score - 1.0 / (RRF_K + 1.0)).abs() < 1e-12);
    }

    #[test]
    fn fusion_interleaves_tiers_where_neither_alone_wins() {
        // fts5 ranks x first; vec0 ranks y first; z is second in both.
        let fused = reciprocal_rank_fusion(
            &[
                list("fts5", 1.0, vec![hit("x.md", 1), hit("z.md", 1)]),
                list("vec0", 1.0, vec![hit("y.md", 1), hit("z.md", 1)]),
            ],
            RRF_K,
            10,
        );
        assert_eq!(
            fused[0].result.file,
            PathBuf::from("z.md"),
            "multi-signal agreement outranks any single-tier leader"
        );
    }

    #[test]
    fn zero_weight_tier_is_ignored() {
        let fused = reciprocal_rank_fusion(
            &[
                list("fts5", 1.0, vec![hit("a.md", 1)]),
                list("graph", 0.0, vec![hit("b.md", 1)]),
            ],
            RRF_K,
            10,
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].result.file, PathBuf::from("a.md"));
    }

    #[test]
    fn empty_lists_fuse_to_empty() {
        assert!(reciprocal_rank_fusion(&[], RRF_K, 10).is_empty());
        assert!(reciprocal_rank_fusion(&[list("fts5", 1.0, vec![])], RRF_K, 10).is_empty());
    }

    #[test]
    fn limit_truncates_best_first() {
        let fused = reciprocal_rank_fusion(
            &[list("fts5", 1.0, vec![hit("a.md", 1), hit("b.md", 1)])],
            RRF_K,
            1,
        );
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].result.file, PathBuf::from("a.md"));
    }

    #[test]
    fn single_tier_order_is_preserved() {
        let fused = reciprocal_rank_fusion(
            &[list(
                "fts5",
                1.0,
                vec![hit("a.md", 1), hit("b.md", 1), hit("c.md", 1)],
            )],
            RRF_K,
            10,
        );
        let files: Vec<_> = fused.iter().map(|f| f.result.file.clone()).collect();
        assert_eq!(
            files,
            vec![
                PathBuf::from("a.md"),
                PathBuf::from("b.md"),
                PathBuf::from("c.md")
            ]
        );
    }
}
