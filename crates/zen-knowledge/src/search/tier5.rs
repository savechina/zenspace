use anyhow::Result;

use super::SearchResult;

/// Phase 4: LLM synthesis interface (FR-011), deferred to zen-provider integration
///
/// TODO: Implement LLM-powered synthesis over search results,
/// context aggregation, and structured response generation.
#[derive(Debug)]
pub struct Tier5Search;

impl Tier5Search {
    /// LLM synthesis over combined search results.
    ///
    /// STUB: Returns deferred message. Phase 4 implementation pending.
    pub fn synthesize(
        &self,
        query: &str,
        context: &[SearchResult],
    ) -> Result<String, anyhow::Error> {
        tracing::info!(
            "Tier5 LLM synthesis stub: query={query}, context_items={}",
            context.len()
        );
        Ok("LLM synthesis deferred — zen-provider integration pending".to_string())
    }
}
