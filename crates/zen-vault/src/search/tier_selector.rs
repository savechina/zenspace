/// TierSelector determines the optimal search tier based on query characteristics.
///
/// Maps queries to one of 5 tiers:
/// - Tier 1: ripgrep exact match (single word)
/// - Tier 2: FTS5 full-text search (multi-word)
/// - Tier 3: vector similarity search
/// - Tier 4: notion graph traversal
/// - Tier 5: LLM synthesis
pub struct TierSelector;

impl TierSelector {
    /// Select search tier based on query characteristics alone.
    ///
    /// # Tier selection rules
    /// - `"similar:"` or `"like:"` prefix → Tier 3 (vector)
    /// - `"graph:"` or `"related:"` prefix → Tier 4 (notion graph)
    /// - `"summarize:"` or `"explain:"` prefix → Tier 5 (LLM synthesis)
    /// - Single word (no whitespace) → Tier 1 (ripgrep)
    /// - Multiple words → Tier 2 (FTS5)
    /// - Default → Tier 2
    pub fn select_tier(query: &str) -> u8 {
        let trimmed = query.trim();
        let lower = trimmed.to_lowercase();
        if lower.starts_with("similar:") || lower.starts_with("like:") {
            return 3;
        }
        if lower.starts_with("graph:") || lower.starts_with("related:") {
            return 4;
        }
        if lower.starts_with("summarize:") || lower.starts_with("explain:") {
            return 5;
        }
        if trimmed.split_whitespace().count() <= 1 {
            return 1;
        }
        2
    }
}

#[cfg(test)]
mod tests {
    use super::TierSelector;

    #[test]
    fn test_single_word_returns_tier1() {
        assert_eq!(TierSelector::select_tier("rust"), 1);
        assert_eq!(TierSelector::select_tier("  hello  "), 1);
    }

    #[test]
    fn test_multi_word_returns_tier2() {
        assert_eq!(TierSelector::select_tier("rust programming"), 2);
        assert_eq!(TierSelector::select_tier("how to use zen"), 2);
    }

    #[test]
    fn test_prefix_selection() {
        assert_eq!(TierSelector::select_tier("similar: embeddings"), 3);
        assert_eq!(TierSelector::select_tier("like: vectors"), 3);
        assert_eq!(TierSelector::select_tier("graph: notions"), 4);
        assert_eq!(TierSelector::select_tier("related: concepts"), 4);
        assert_eq!(TierSelector::select_tier("summarize: this"), 5);
        assert_eq!(TierSelector::select_tier("explain: the code"), 5);
    }

    #[test]
    fn test_prefix_case_insensitive() {
        assert_eq!(TierSelector::select_tier("Similar: embeddings"), 3);
        assert_eq!(TierSelector::select_tier("GRAPH: notions"), 4);
        assert_eq!(TierSelector::select_tier("Explain: the code"), 5);
    }

    #[test]
    fn test_empty_query() {
        assert_eq!(TierSelector::select_tier(""), 1);
        assert_eq!(TierSelector::select_tier("  "), 1);
    }
}
