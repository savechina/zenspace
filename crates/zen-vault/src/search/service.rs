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

/// Per-tier RRF weights for fused auto-search. FTS5 and vec0 are independent
/// retrieval signals (lexical vs semantic) and share full trust; graph
/// traversal is directional/relation-driven, hence the lower weight.
const TIER2_WEIGHT: f64 = 1.0;
const TIER3_WEIGHT: f64 = 1.0;
const TIER4_WEIGHT: f64 = 0.8;

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
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);

        let embedding = match crate::tindy::compute_embeddings_for_text(query) {
            Ok(embedding) => Some(embedding),
            Err(e) => {
                warn!(
                    error = %e,
                    "tier3 (vec0) unavailable for fusion: query embedding failed"
                );
                None
            }
        };

        let (fts, semantic, graph) = tokio::join!(
            self.tier2.search_in_dir(client, query, base_dir, limit),
            async {
                match &embedding {
                    Some(embedding) => self.tier3.search(client, embedding, limit).await,
                    None => Ok(Vec::new()),
                }
            },
            self.tier4.search(client, query, 3),
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

        let fused = reciprocal_rank_fusion(&lists, RRF_K, limit);
        info!(
            query_len = query.len(),
            contributing_tiers = lists.len(),
            results_count = fused.len(),
            "SearchService: fused search complete"
        );
        Ok(fused)
    }

    /// Search across all tiers.
    ///
    /// If `tier` is `Some`, uses that tier directly (and `tier = 5` runs
    /// LLM synthesis).
    /// If `tier` is `None`, [`TierSelector::select_tier`] decides; the
    /// ordinary multi-word case (selected tier 2) is served by
    /// [`SearchService::search_fused`] (RRF over FTS5 + vec0 + graph) instead
    /// of FTS5 alone. Explicit intents (`similar:` / `graph:` / `summarize:`)
    /// and single-word lookups keep their single-tier routing.
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
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let selected = match tier {
            Some(explicit) => explicit,
            None => {
                let preferred = TierSelector::select_tier(query);
                if preferred == 2 {
                    let fused = self
                        .search_fused(query, base_dir, client, Some(limit))
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
                    return Ok(results);
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
                let query_embedding = crate::tindy::compute_embeddings_for_text(query)
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Tier 3 search failed: embedding computation error for query ({} chars): {e}",
                            query.len()
                        )
                    })?;
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

        Ok(results)
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
