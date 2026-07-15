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

    /// Auto-select tier considering available data sources.
    ///
    /// Falls back to lower tiers if higher-tier data sources are unavailable:
    /// - Tier 5 falls back to Tier 2 (LLM not a data source, always available)
    /// - Tier 4 falls back to Tier 2 if `has_graph` is false
    /// - Tier 3 falls back to Tier 2 if `has_embeddings` is false
    /// - Tiers 1 and 2 are always available
    pub fn auto_select(query: &str, has_embeddings: bool, has_graph: bool) -> u8 {
        let preferred = Self::select_tier(query);

        match preferred {
            3 if !has_embeddings => 2,
            4 if !has_graph => 2,
            _ => preferred,
        }
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
    fn test_auto_select_with_all_sources() {
        assert_eq!(TierSelector::auto_select("rust", true, true), 1);
        assert_eq!(TierSelector::auto_select("rust lang", true, true), 2);
        assert_eq!(TierSelector::auto_select("similar: x", true, true), 3);
        assert_eq!(TierSelector::auto_select("graph: x", true, true), 4);
        assert_eq!(TierSelector::auto_select("summarize: x", true, true), 5);
    }

    #[test]
    fn test_auto_select_falls_back_without_sources() {
        assert_eq!(TierSelector::auto_select("similar: x", false, true), 2);
        assert_eq!(TierSelector::auto_select("graph: x", true, false), 2);
        assert_eq!(TierSelector::auto_select("similar: x", false, false), 2);
        assert_eq!(TierSelector::auto_select("graph: x", false, false), 2);
    }

    #[test]
    fn test_auto_select_ignores_fallback_for_tier5() {
        assert_eq!(TierSelector::auto_select("summarize: x", false, false), 5);
    }

    #[test]
    fn test_empty_query() {
        assert_eq!(TierSelector::select_tier(""), 1);
        assert_eq!(TierSelector::select_tier("  "), 1);
    }
}
