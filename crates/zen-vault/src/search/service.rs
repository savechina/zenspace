use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::{info, warn};
use zen_repo::SqliteClient;

use super::{
    FusedResult, GraphResult, RRF_K, RankedList, SearchResult, Tier1Search, Tier2Search,
    Tier3Search, Tier4Search, Tier5Search, TierSelector, reciprocal_rank_fusion,
};
use zen_provider::DefaultRouter;

/// Unified search service that routes queries to the appropriate tier.
#[derive(Debug)]
pub struct SearchService {
    tier2: Tier2Search,
    tier3: Tier3Search,
    tier4: Tier4Search,
    tier5: Tier5Search,
}

/// Default result cap applied to each tier when the caller does not specify
/// a limit (`zen search --limit`).
const DEFAULT_SEARCH_LIMIT: usize = 20;

/// Per-tier RRF weights for fused auto-search. Ripgrep and FTS5 are lexical
/// signals of the same trust class (exact text) and share full weight —
/// ripgrep scans file CONTENT directly, so it stays independent of index
/// freshness; vec0 is the independent semantic signal; graph traversal is
/// directional/relation-driven, hence the lower weight. PageRank seeding is
/// graph-derived too, but query-relevance ranked rather than
/// path-determined, so it sits between the two.
const TIER1_WEIGHT: f64 = 1.0;
const TIER2_WEIGHT: f64 = 1.0;
const TIER3_WEIGHT: f64 = 1.0;
const TIER4_WEIGHT: f64 = 0.8;
const PPR_WEIGHT: f64 = 0.9;

/// How the **query** embedding was obtained, for callers that must surface a
/// silent degradation (O16). Both flags are `false` on the healthy path, so
/// ignoring this struct is byte-identical to the pre-O16 code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchDiagnostics {
    /// True when the query fell back to the deterministic local hash vector
    /// (no embedding provider reachable). The vec0 list is still fused as a
    /// retrieval fallback, but it is not semantic — this is what makes that
    /// visible instead of log-only.
    pub query_embedding_degraded: bool,
    /// True when the query embedding could not be computed at all, so the vec0
    /// list was skipped (previously silent on the fused path).
    pub query_embedding_unavailable: bool,
}

impl SearchService {
    pub fn new(router: DefaultRouter) -> Self {
        Self {
            tier2: Tier2Search,
            tier3: Tier3Search,
            tier4: Tier4Search,
            tier5: Tier5Search::new(router),
        }
    }

    /// Fused multi-signal search (RRF): runs the indexed tiers in parallel —
    /// FTS5 (lexical), vec0 (semantic, when embeddings are computable) and
    /// the entity graph — then fuses their rankings.
    ///
    /// Tier failures degrade gracefully: a failing or unavailable tier
    /// contributes nothing and is warn-logged; the remaining tiers still
    /// produce a ranked result. Hits carry provenance (which tiers matched)
    /// so callers can explain ranking.
    pub async fn search_fused(
        &self,
        query: &str,
        base_dir: &Path,
        client: &SqliteClient,
        limit: Option<usize>,
    ) -> Result<Vec<FusedResult>> {
        Ok(self
            .search_fused_with_diagnostics(query, base_dir, client, limit)
            .await?
            .0)
    }

    /// [`SearchService::search_fused`] plus query-embedding diagnostics (O16);
    /// the fused list and its ordering are identical.
    pub async fn search_fused_with_diagnostics(
        &self,
        query: &str,
        base_dir: &Path,
        client: &SqliteClient,
        limit: Option<usize>,
    ) -> Result<(Vec<FusedResult>, SearchDiagnostics)> {
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let mut diagnostics = SearchDiagnostics::default();

        let query_owned = query.to_string();
        // Owned payload on a blocking thread: the embedding providers are sync
        // and a dead/hung one would otherwise park a tokio worker for its full
        // (240s) timeout. spawn_blocking (unlike a dropped future) runs the
        // call to completion on the blocking pool even if this task is
        // cancelled. A join failure is treated exactly like a compute error.
        let embedding = match tokio::task::spawn_blocking(move || {
            crate::tindy::compute_embeddings_for_text_tagged(&query_owned)
        })
        .await
        {
            Ok(Ok((embedding, degraded))) => {
                diagnostics.query_embedding_degraded = degraded;
                Some(embedding)
            }
            Ok(Err(e)) => {
                diagnostics.query_embedding_unavailable = true;
                warn!(
                    error = %e,
                    "tier3 (vec0) unavailable for fusion: query embedding failed"
                );
                None
            }
            Err(e) => {
                diagnostics.query_embedding_unavailable = true;
                warn!(
                    error = %e,
                    "tier3 (vec0) unavailable for fusion: query embedding task failed"
                );
                None
            }
        };

        let (fts, semantic, graph, ppr, raw) = tokio::join!(
            self.tier2.search_in_dir(client, query, base_dir, limit),
            async {
                match &embedding {
                    Some(embedding) => self.tier3.search(client, embedding, limit).await,
                    None => Ok(Vec::new()),
                }
            },
            self.tier4.search(client, query, 3),
            self.tier4.seeded_ranking(client, query, limit),
            async {
                let query = query.to_string();
                let dir = base_dir.to_path_buf();
                tokio::task::spawn_blocking(move || Tier1Search::search(&query, &dir, limit))
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("tier1 (ripgrep) join failed: {e}")))
            },
        );

        let mut lists = Vec::new();
        match fts {
            Ok(rows) => lists.push(RankedList {
                source: "fts5",
                weight: TIER2_WEIGHT,
                results: rows
                    .into_iter()
                    .map(|f| SearchResult {
                        file: PathBuf::from(f.path),
                        line: 0,
                        content: f.snippet,
                    })
                    .collect(),
            }),
            Err(e) => warn!(error = %e, "tier2 (fts5) failed during fusion"),
        }
        match semantic {
            Ok(rows) if !rows.is_empty() => lists.push(RankedList {
                source: "vec0",
                weight: TIER3_WEIGHT,
                results: rows,
            }),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "tier3 (vec0) failed during fusion"),
        }
        match graph {
            Ok(rows) => lists.push(RankedList {
                source: "graph",
                weight: TIER4_WEIGHT,
                results: rows.into_iter().map(graph_to_search).collect(),
            }),
            Err(e) => warn!(error = %e, "tier4 (graph) failed during fusion"),
        }
        match ppr {
            Ok(rows) if !rows.is_empty() => lists.push(RankedList {
                source: "ppr",
                weight: PPR_WEIGHT,
                results: rows,
            }),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "tier4 (ppr seeding) failed during fusion"),
        }
        match raw {
            Ok(rows) if !rows.is_empty() => lists.push(RankedList {
                source: "ripgrep",
                weight: TIER1_WEIGHT,
                results: rows,
            }),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "tier1 (ripgrep) failed during fusion"),
        }

        let fused = reciprocal_rank_fusion(&lists, RRF_K, limit);
        info!(
            query_len = query.len(),
            contributing_tiers = lists.len(),
            results_count = fused.len(),
            query_embedding_degraded = diagnostics.query_embedding_degraded,
            "SearchService: fused search complete"
        );
        Ok((fused, diagnostics))
    }

    /// Search across all tiers.
    ///
    /// If `tier` is `Some`, uses that tier directly (and `tier = 5` runs
    /// LLM synthesis).
    /// If `tier` is `None`, [`TierSelector::select_tier`] decides; ordinary
    /// queries (selected tier 1 or 2 — single-word AND multi-word) are
    /// served by [`SearchService::search_fused`] (RRF over ripgrep + FTS5 +
    /// vec0 + graph) so every auto search is content-scanning AND semantic.
    /// Explicit intents (`similar:` / `graph:` / `summarize:`) keep their
    /// single-tier routing.
    ///
    /// `limit` caps the number of results returned by each tier, defaulting
    /// to [`DEFAULT_SEARCH_LIMIT`].
    pub async fn search(
        &self,
        query: &str,
        base_dir: &Path,
        client: &SqliteClient,
        tier: Option<u8>,
        domain_filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchResult>> {
        Ok(self
            .search_with_diagnostics(query, base_dir, client, tier, domain_filter, limit)
            .await?
            .0)
    }

    /// [`SearchService::search`] plus query-embedding diagnostics (O16);
    /// routing and results are identical.
    pub async fn search_with_diagnostics(
        &self,
        query: &str,
        base_dir: &Path,
        client: &SqliteClient,
        tier: Option<u8>,
        domain_filter: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<SearchResult>, SearchDiagnostics)> {
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let selected = match tier {
            Some(explicit) => explicit,
            None => {
                let preferred = TierSelector::select_tier(query);
                // Single-word used to fall through to bare tier-1 ripgrep,
                // silently skipping the semantic (vec0) and graph signals.
                if matches!(preferred, 1 | 2) {
                    let (fused, diagnostics) = self
                        .search_fused_with_diagnostics(query, base_dir, client, Some(limit))
                        .await?;
                    let results: Vec<SearchResult> = fused.into_iter().map(|f| f.result).collect();
                    let results = match domain_filter {
                        Some(domain) => filter_by_domain(results, domain)?,
                        None => results,
                    };
                    info!(
                        query_len = query.len(),
                        results_count = results.len(),
                        "SearchService: fused auto search complete"
                    );
                    return Ok((results, diagnostics));
                }
                preferred
            }
        };

        info!(
            query_len = query.len(),
            tier = selected,
            limit = limit,
            "SearchService: routing query"
        );

        let mut query_embedding_degraded = false;
        let results = match selected {
            1 => Tier1Search::search(query, base_dir, limit),
            2 => self
                .tier2
                .search_in_dir(client, query, base_dir, limit)
                .await
                .map(|r| {
                    r.into_iter()
                        .map(|f| SearchResult {
                            file: PathBuf::from(f.path),
                            line: 0,
                            content: f.snippet,
                        })
                        .collect()
                }),
            3 => {
                let query_owned = query.to_string();
                // Same blocking-thread discipline as the fused path above: the
                // sync embedding call must never park this worker.
                let (query_embedding, degraded) = tokio::task::spawn_blocking(move || {
                    crate::tindy::compute_embeddings_for_text_tagged(&query_owned)
                })
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Tier 3 search failed: query embedding task join error: {e}")
                })?
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Tier 3 search failed: embedding computation error for query ({} chars): {e}",
                        query.len()
                    )
                })?;
                query_embedding_degraded = degraded;
                self.tier3.search(client, &query_embedding, limit).await
            }
            4 => self
                .tier4
                .search(client, query, 3)
                .await
                .map(|graphs| graphs.into_iter().map(graph_to_search).collect()),
            5 => {
                let mut context = self
                    .tier2
                    .search_in_dir(client, query, base_dir, limit)
                    .await
                    .map(|r| {
                        r.into_iter()
                            .map(|f| SearchResult {
                                file: PathBuf::from(f.path),
                                line: 0,
                                content: f.snippet,
                            })
                            .collect::<Vec<_>>()
                    })?;
                // FR-031 Sub-graph Synthesis RAG: prepend the N-hop entity
                // neighborhood so synthesis reasons over the local graph
                // structure, not just Top-K text hits. No matching entity →
                // empty subgraph → no injection.
                if let Ok(subgraph) = Tier4Search::subgraph_synthesis(client, query, 2).await
                    && subgraph.nodes.len() > 1
                {
                    context.insert(
                        0,
                        SearchResult {
                            file: PathBuf::from(format!("@subgraph:{}", subgraph.center)),
                            line: 0,
                            content: subgraph.render(),
                        },
                    );
                }
                Ok(match self.tier5.synthesize(query, &context) {
                    Ok(synthesized) => vec![SearchResult {
                        file: PathBuf::from("synthesis"),
                        line: 0,
                        content: synthesized,
                    }],
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Tier5 synthesis failed, returning raw context results"
                        );
                        context
                    }
                })
            }
            _ => anyhow::bail!("unknown tier: {selected}"),
        }?;

        let results = if let Some(domain) = domain_filter {
            filter_by_domain(results, domain)?
        } else {
            results
        };

        info!(
            query_len = query.len(),
            tier = selected,
            results_count = results.len(),
            "SearchService: search complete"
        );

        Ok((
            results,
            SearchDiagnostics {
                query_embedding_degraded,
                ..SearchDiagnostics::default()
            },
        ))
    }

    /// Synthesize search results into a natural language answer.
    pub fn synthesize(
        &self,
        query: &str,
        results: &[SearchResult],
    ) -> Result<String, anyhow::Error> {
        self.tier5.synthesize(query, results)
    }
}

pub(crate) fn filter_by_domain(
    results: Vec<SearchResult>,
    domain: &str,
) -> Result<Vec<SearchResult>> {
    use std::collections::HashMap;

    let domain_lower = domain.to_lowercase();
    let mut domain_cache: HashMap<PathBuf, bool> = HashMap::new();
    let mut filtered = Vec::new();
    for r in results {
        // Graph/entity results use a synthetic `@notion` path; they carry no
        // file frontmatter to filter on, so keep them.
        if r.file.to_string_lossy().starts_with('@') {
            filtered.push(r);
            continue;
        }
        let has_domain = *domain_cache.entry(r.file.clone()).or_insert_with(|| {
            std::fs::read_to_string(&r.file)
                .ok()
                .and_then(|content| crate::note::parse_frontmatter(&content).ok())
                .map(|note| note.domain.iter().any(|d| d.to_string() == domain_lower))
                .unwrap_or(false)
        });
        if has_domain {
            filtered.push(r);
        }
    }
    Ok(filtered)
}

fn graph_to_search(g: GraphResult) -> SearchResult {
    SearchResult {
        file: PathBuf::from(format!("@{}", g.notion)),
        line: g.depth,
        content: format!("{} → {}", g.relation, g.target),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_client(dir: &Path) -> SqliteClient {
        zen_repo::SqliteClient::open(&dir.join("state.db"))
            .await
            .expect("open temp state.db")
    }

    fn test_service() -> SearchService {
        SearchService::new(zen_provider::DefaultRouter::new(
            zen_provider::LlmConfig::default(),
        ))
    }

    /// Fusion must include a raw content scan (tier-1): a file that
    /// exists on disk but was never indexed into FTS/vec0 is still findable.
    #[tokio::test]
    async fn fused_search_scans_unindexed_files_via_ripgrep() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("unindexed.md"),
            "buffett letters archive reading notes\n",
        )
        .unwrap();
        let client = temp_client(tmp.path()).await;
        let fused = test_service()
            .search_fused("buffett", tmp.path(), &client, Some(10))
            .await
            .unwrap();
        assert!(
            fused
                .iter()
                .any(|f| f.result.file.ends_with("unindexed.md")),
            "ripgrep list must surface unindexed vault content, got {fused:?}"
        );
    }

    /// Single-word auto queries must route through fusion, not bare
    /// tier-1: a STALE index row (FTS content contains the word, the on-disk
    /// file does not — ripgrep is blind to it) is still returned via the
    /// FTS5 list. Bare tier-1 routing would return empty here.
    #[tokio::test]
    async fn single_word_auto_query_routes_through_fusion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let stale = tmp.path().join("indexed.md");
        std::fs::write(&stale, "plain text nothing special\n").unwrap();
        let client = temp_client(tmp.path()).await;
        zen_repo::NotesRepo::new(&client)
            .index_note(zen_repo::types::IndexNoteRequest {
                id: "n1",
                title: "Stale row",
                content: "xylophone zanzibar quixotic",
                tags: "[]",
                file_path: stale.to_str().unwrap(),
                source: "test",
            })
            .await
            .unwrap();
        let results = test_service()
            .search("xylophone", tmp.path(), &client, None, None, Some(10))
            .await
            .unwrap();
        assert!(
            results.iter().any(|r| r.file.ends_with("indexed.md")),
            "single-word query must reach the FTS list via fusion, got {results:?}"
        );
    }

    fn fused_triples(fused: &[FusedResult]) -> Vec<(std::path::PathBuf, u32, String)> {
        fused
            .iter()
            .map(|f| {
                (
                    f.result.file.clone(),
                    f.result.line,
                    f.result.content.clone(),
                )
            })
            .collect()
    }

    /// O16: the query-side embedding state is reported instead of being
    /// log-only, and the diagnostics-returning entry point is byte-identical to
    /// the plain one (the flag is additive — it changes no ranking).
    ///
    /// The expected flag is derived from a live probe rather than asserted as a
    /// host property: a box with a reachable embedding provider legitimately
    /// reports `false`, one without reports `true` (O15 post-mortem).
    #[tokio::test]
    async fn fused_diagnostics_match_a_live_probe_and_the_plain_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("note.md"), "buffett letters archive\n").unwrap();
        let client = temp_client(tmp.path()).await;
        let svc = test_service();

        let (_, probe_degraded) =
            crate::tindy::compute_embeddings_for_text_tagged("buffett").unwrap();

        let plain = svc
            .search_fused("buffett", tmp.path(), &client, Some(10))
            .await
            .unwrap();
        let (with_diagnostics, diagnostics) = svc
            .search_fused_with_diagnostics("buffett", tmp.path(), &client, Some(10))
            .await
            .unwrap();

        assert_eq!(
            diagnostics.query_embedding_degraded, probe_degraded,
            "the fused path must report the same query-embedding state as a direct probe"
        );
        assert!(
            !diagnostics.query_embedding_unavailable,
            "a probe that succeeded means no embedding was unavailable"
        );
        assert_eq!(
            fused_triples(&plain),
            fused_triples(&with_diagnostics),
            "reporting diagnostics must not change the fused ranking"
        );
    }

    #[tokio::test]
    async fn explicit_vec_tier_diagnostics_match_a_live_probe() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("note.md"), "buffett letters\n").unwrap();
        let client = temp_client(tmp.path()).await;
        let (_, probe_degraded) =
            crate::tindy::compute_embeddings_for_text_tagged("buffett").unwrap();

        let (_, diagnostics) = test_service()
            .search_with_diagnostics("buffett", tmp.path(), &client, Some(3), None, Some(10))
            .await
            .unwrap();

        assert_eq!(
            diagnostics.query_embedding_degraded, probe_degraded,
            "the explicit vec tier must report the same state as a direct probe"
        );
    }

    /// The healthy default is what the thin wrappers hand to callers that
    /// ignore diagnostics, so both flags must start `false`.
    #[test]
    fn diagnostics_default_is_healthy() {
        let d = SearchDiagnostics::default();
        assert!(!d.query_embedding_degraded);
        assert!(!d.query_embedding_unavailable);
    }

    /// A degraded query embedding must not close the lexical tiers: in unit
    /// tests the embedding router is empty, so every query degrades to the
    /// hash fallback — Given: one stale-indexed FTS row (invisible to ripgrep) and one
    /// unindexed on-disk file (invisible to FTS).
    /// When: the fused pipeline runs with the degraded query vector.
    /// Then: both files surface through fusion AND the diagnostics say
    /// degraded — proving degradation only gates vec0, never tier1/tier2.
    #[tokio::test]
    async fn degraded_query_embedding_still_fuses_lexical_hits() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("unindexed.md"),
            "buffett letters archive reading notes\n",
        )
        .unwrap();
        let stale = tmp.path().join("indexed.md");
        std::fs::write(&stale, "plain text nothing special\n").unwrap();
        let client = temp_client(tmp.path()).await;
        zen_repo::NotesRepo::new(&client)
            .index_note(zen_repo::types::IndexNoteRequest {
                id: "n1",
                title: "Stale row",
                content: "xylophone zanzibar quixotic",
                tags: "[]",
                file_path: stale.to_str().unwrap(),
                source: "test",
            })
            .await
            .unwrap();

        let (fused, diagnostics) = test_service()
            .search_fused_with_diagnostics("xylophone", tmp.path(), &client, Some(10))
            .await
            .unwrap();

        assert!(
            diagnostics.query_embedding_degraded,
            "unit-test env has no embedding provider: query must run degraded"
        );
        assert!(
            !diagnostics.query_embedding_unavailable,
            "the infallible hash fallback means the flag stays false"
        );
        assert!(
            fused.iter().any(|f| f.result.file.ends_with("indexed.md")),
            "FTS5 lexical hits must survive a degraded query embedding, got {fused:?}"
        );
    }
}
