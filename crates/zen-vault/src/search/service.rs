use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

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
    pub fn search(
        &self,
        query: &str,
        base_dir: &Path,
        tier: Option<u8>,
    ) -> Result<Vec<SearchResult>> {
        let selected = tier.unwrap_or_else(|| TierSelector::select_tier(query));

        info!(
            query_len = query.len(),
            tier = selected,
            "SearchService: routing query"
        );

        let db_dir = base_dir.parent().unwrap_or(base_dir);
        let kb_db = db_dir.join("kb.db");
        let vec_db = db_dir.join("vec.db");
        let graph_db = db_dir.join("graph.db");

        let results = match selected {
            1 => Tier1Search::search(query, base_dir),
            2 => self.tier2.search(query, &kb_db, 20).map(|r| {
                r.into_iter()
                    .map(|f| SearchResult {
                        file: PathBuf::from(f.path),
                        line: 0,
                        content: f.snippet,
                    })
                    .collect()
            }),
            3 => self.tier3.search(&[], &vec_db, 10),
            4 => self
                .tier4
                .search(query, &graph_db, 3)
                .map(|graphs| graphs.into_iter().map(graph_to_search).collect()),
            5 => {
                // Get tier 2 results as context for LLM synthesis
                let context = self.tier2.search(query, &kb_db, 20).map(|r| {
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

fn graph_to_search(g: GraphResult) -> SearchResult {
    SearchResult {
        file: PathBuf::from(format!("@{}", g.entity)),
        line: g.depth,
        content: format!("{} → {}", g.relation, g.target),
    }
}
