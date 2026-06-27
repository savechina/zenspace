use anyhow::{Context, Result};

use super::SearchResult;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouterExt};

/// Tier 5: LLM synthesis over search results (FR-011).
///
/// Takes Tier1-4 search results + original query → synthesizes natural-language
/// answer via LLM call. Only invoked for complex/reasoning queries or when the
/// user explicitly requests synthesis via `summarize:` / `explain:` prefix.
#[derive(Debug)]
pub struct Tier5Search {
    router: DefaultRouter,
}

impl Tier5Search {
    pub fn new(router: DefaultRouter) -> Self {
        Self { router }
    }

    /// Synthesize search results into a natural language answer.
    ///
    /// **Phase 1 — cost guard**: If ≥3 non-empty results, skip LLM and return
    /// a hint to use `/synthesize` explicitly. This prevents unnecessary LLM
    /// calls when local tiers already provide sufficient coverage.
    ///
    /// **Phase 2 — prompt assembly**: Build a structured prompt from the query
    /// and search results, with clear instructions for the LLM to cite sources
    /// and stay concise.
    ///
    /// **Phase 3 — LLM call**: Delegates to [`LlmRouterExt::complete`] with
    /// `Sensitivity::Public` (synthesis is over public knowledge base content).
    pub fn synthesize(
        &self,
        query: &str,
        context: &[SearchResult],
    ) -> Result<String, anyhow::Error> {
        // Phase 1: Cost guard — skip LLM when local tiers suffice
        let high_confidence_count = context
            .iter()
            .filter(|r| !r.content.is_empty())
            .count();

        if high_confidence_count >= 3 {
            tracing::info!(
                high_confidence_count = high_confidence_count,
                "Tier5 cost guard: sufficient local results, hinting /synthesize"
            );
            return Ok(
                "Local search found sufficient results. Use `/synthesize` for AI summary."
                    .to_string(),
            );
        }

        // Phase 2: Build prompt with numbered source references
        let context_text = context
            .iter()
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "[{}] {}:{}\n{}",
                    i + 1,
                    r.file.display(),
                    r.line,
                    r.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "You are a knowledge synthesis assistant. Given a user query and search results \
             from a local knowledge base, synthesize a concise, accurate answer.\n\n\
             Query: {query}\n\n\
             Search Results:\n{context_text}\n\n\
             Instructions:\n\
             - Answer based ONLY on the provided search results\n\
             - If the results don't contain enough information, say so\n\
             - Cite sources by filename where possible\n\
             - Be concise (2-3 paragraphs max unless the query demands more)",
            query = query,
            context_text = context_text
        );

        tracing::info!(
            query_len = query.len(),
            context_items = context.len(),
            prompt_len = prompt.len(),
            "Tier5 synthesize: calling LLM"
        );

        // Phase 3: LLM call via LlmRouterExt::complete
        let response = self
            .router
            .complete("synthesis", &prompt, Sensitivity::Public)
            .context("Tier5 LLM synthesis failed")?;

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Returns a DefaultRouter pre-configured with a MockProvider.
    fn mock_router() -> DefaultRouter {
        use zen_core::config::{ProviderConfig, ZenConfig};

        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "synthesis-test".into(),
            ProviderConfig {
                provider_type: Some("mock".into()),
                ..ProviderConfig::default()
            },
        );

        DefaultRouter::from_agentic(&ZenConfig {
            default_provider: Some("synthesis-test".into()),
            providers,
            ..ZenConfig::default()
        })
    }

    fn search_result(file: &str, line: u32, content: &str) -> SearchResult {
        SearchResult {
            file: PathBuf::from(file),
            line,
            content: content.to_string(),
        }
    }

    // ------------------------------------------------------------------
    // Cost guard tests
    // ------------------------------------------------------------------

    #[test]
    fn test_cost_guard_skips_llm_when_three_or_more_results() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results = vec![
            search_result("a.md", 1, "content a"),
            search_result("b.md", 2, "content b"),
            search_result("c.md", 3, "content c"),
        ];

        let answer = tier5.synthesize("test query", &results).unwrap();
        assert!(
            answer.contains("/synthesize"),
            "Expected /synthesize hint, got: {answer}"
        );
    }

    #[test]
    fn test_cost_guard_allows_llm_with_two_results() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results = vec![
            search_result("a.md", 1, "content a"),
            search_result("b.md", 2, "content b"),
        ];

        let answer = tier5.synthesize("test query", &results).unwrap();
        // With <3 results and mock provider, we get a mock response — not the hint
        assert!(
            !answer.contains("/synthesize"),
            "Should not contain /synthesize hint, got: {answer}"
        );
    }

    #[test]
    fn test_cost_guard_counts_only_non_empty_content() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        // Two non-empty + one empty = 2 high-confidence → should NOT trigger cost guard
        let results = vec![
            search_result("a.md", 1, "content a"),
            search_result("b.md", 2, ""),
            search_result("c.md", 3, "content c"),
        ];

        let answer = tier5.synthesize("test query", &results).unwrap();
        assert!(
            !answer.contains("/synthesize"),
            "Empty content should not count toward cost guard threshold, got: {answer}"
        );
    }

    #[test]
    fn test_cost_guard_triggers_on_exactly_three_results() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results = vec![
            search_result("a.md", 1, "x"),
            search_result("b.md", 2, "y"),
            search_result("c.md", 3, "z"),
        ];

        let answer = tier5.synthesize("test", &results).unwrap();
        assert!(answer.contains("/synthesize"));
    }

    // ------------------------------------------------------------------
    // LLM call path tests
    // ------------------------------------------------------------------

    #[test]
    fn test_synthesize_calls_mock_llm_with_few_results() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results = vec![search_result("a.md", 1, "hello world")];

        let answer = tier5.synthesize("my query", &results).unwrap();
        // Mock returns: "[mock] task=call prompt_len=N reply=..."
        // Verify the call reaches the LLM (not the cost-guard hint)
        assert!(
            answer.contains("mock"),
            "Expected mock LLM response, got: {answer}"
        );
    }

    #[test]
    fn test_synthesize_graceful_empty_context() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results: Vec<SearchResult> = vec![];

        // Should not panic or error on empty context (already <3, goes to LLM)
        let result = tier5.synthesize("query", &results);
        assert!(result.is_ok());
    }

    #[test]
    fn test_synthesize_empty_query() {
        let router = mock_router();
        let tier5 = Tier5Search::new(router);
        let results = vec![search_result("a.md", 1, "content")];

        let result = tier5.synthesize("", &results);
        assert!(result.is_ok(), "Empty query should not crash: {result:?}");
    }
}
