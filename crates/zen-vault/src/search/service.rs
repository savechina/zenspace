use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;
use zen_repo::SqliteClient;

use super::{
    GraphResult, SearchResult, Tier1Search, Tier2Search, Tier3Search, Tier4Search, Tier5Search,
    TierSelector,
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

impl SearchService {
    pub fn new(router: DefaultRouter) -> Self {
        Self {
            tier2: Tier2Search,
            tier3: Tier3Search,
            tier4: Tier4Search,
            tier5: Tier5Search::new(router),
        }
    }

    /// Search across all tiers.
    ///
    /// If `tier` is `Some`, uses that tier directly.
    /// If `tier` is `None`, uses [`TierSelector::select_tier`] to auto-select.
    pub async fn search(
        &self,
        query: &str,
        base_dir: &Path,
        client: &SqliteClient,
        tier: Option<u8>,
        domain_filter: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let selected = tier.unwrap_or_else(|| TierSelector::select_tier(query));

        info!(
            query_len = query.len(),
            tier = selected,
            "SearchService: routing query"
        );

        let results = match selected {
            1 => Tier1Search::search(query, base_dir),
            2 => self
                .tier2
                .search_in_dir(client, query, base_dir, 20)
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
                self.tier3.search(client, &query_embedding, 10).await
            }
            4 => self
                .tier4
                .search(client, query, 3)
                .await
                .map(|graphs| graphs.into_iter().map(graph_to_search).collect()),
            5 => {
                let context = self
                    .tier2
                    .search_in_dir(client, query, base_dir, 20)
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
                let synthesized = self.tier5.synthesize(query, &context)?;
                Ok(vec![SearchResult {
                    file: PathBuf::from("synthesis"),
                    line: 0,
                    content: synthesized,
                }])
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
    let domain_lower = domain.to_lowercase();
    let mut filtered = Vec::new();
    for r in results {
        if !r.file.exists() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&r.file)
            && let Ok(note) = crate::note::parse_frontmatter(&content)
        {
            let has_domain = note.domain.iter().any(|d| d.to_string() == domain_lower);
            if has_domain {
                filtered.push(r);
            }
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
