//! W2+FUZZY: Reverse-i-search state for the inline TUI.
//!
//! Fuzzy matching via nucleo-matcher (fzf-v2 similarity algorithm, MPL-2.0):
//! case-insensitive substring matches rank above fuzzy-only matches,
//! newest-first deduped results, draft snapshot on enter, restore on cancel.

use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Substring matches always outrank fuzzy matches: score = SUBSTRING_BASE + recency_index.
const SUBSTRING_BASE: u32 = 1_000_000;

/// Reverse-i-search state machine. Owned by `App` as `Option<HistorySearch>`.
/// `None` means search is inactive.
pub struct HistorySearch {
    /// Whether the search overlay is active (footer shows search UI).
    pub active: bool,
    /// Current search query (user-typed characters).
    pub query: String,
    /// Matched history entries (newest-first, deduped by text).
    pub matches: Vec<String>,
    /// Index into `matches` for the currently previewed entry.
    pub match_index: usize,
    /// Snapshot of the textarea content before search started (for cancel).
    pub draft_snapshot: String,
    /// Highlighted character indices per match (char positions, not byte offsets).
    /// Populated by fuzzy/substring matching; empty for entries without indices.
    pub matched_indices: Vec<Vec<usize>>,
    /// Composite sort key per match (score DESC, recency tiebreak).
    pub matched_scores: Vec<u32>,
    /// Reused nucleo-matcher engine (~135KB scratch, created once).
    fuzzy_matcher: Matcher,
}

impl HistorySearch {
    pub fn new() -> Self {
        Self {
            active: false,
            query: String::new(),
            matches: Vec::new(),
            match_index: 0,
            draft_snapshot: String::new(),
            matched_indices: Vec::new(),
            matched_scores: Vec::new(),
            fuzzy_matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Enter search mode, snapshotting the current textarea text.
    pub fn enter(&mut self, current_text: &str) {
        self.active = true;
        self.query.clear();
        self.matches.clear();
        self.match_index = 0;
        self.matched_indices.clear();
        self.matched_scores.clear();
        self.draft_snapshot = current_text.to_string();
    }

    /// Exit search mode, resetting all state.
    pub fn exit(&mut self) {
        self.active = false;
        self.query.clear();
        self.matches.clear();
        self.match_index = 0;
        self.matched_indices.clear();
        self.matched_scores.clear();
        // draft_snapshot preserved until next enter() clears it
    }

    /// Append a character to the query and recompute matches.
    pub fn push_char(&mut self, c: char, history: &[String]) {
        self.query.push(c);
        self.recompute_matches(history);
    }

    /// Remove the last character from the query and recompute matches.
    pub fn backspace(&mut self, history: &[String]) {
        self.query.pop();
        self.recompute_matches(history);
    }

    /// Step to an older match (Ctrl+R or Up while searching).
    pub fn cycle_older(&mut self) {
        if !self.matches.is_empty() {
            self.match_index = (self.match_index + 1).min(self.matches.len() - 1);
        }
    }

    /// Step to a newer match (Down while searching).
    pub fn cycle_newer(&mut self) {
        if self.match_index > 0 {
            self.match_index -= 1;
        }
    }

    /// The currently previewed match, if any.
    pub fn current_match(&self) -> Option<&str> {
        self.matches.get(self.match_index).map(|s| s.as_str())
    }

    /// Highlighted character indices for the currently previewed match.
    /// Returns an empty slice if no indices are available (empty query or no match).
    pub fn current_match_indices(&self) -> &[usize] {
        self.matched_indices
            .get(self.match_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Recompute matches: case-insensitive substring + fuzzy matching via nucleo-matcher.
    ///
    /// Tiered scoring:
    /// - **Substring tier**: candidate contains query (case-insensitive) → score = SUBSTRING_BASE + recency_index.
    ///   Substring matches always outrank fuzzy-only matches.
    /// - **Fuzzy tier**: nucleo-matcher fuzzy_match score (algorithmic, u16→u32).
    /// - No match → excluded.
    ///
    /// Sort: score DESC, recency (newer first) as tiebreak.
    pub(crate) fn recompute_matches(&mut self, history: &[String]) {
        self.matches.clear();
        self.matched_indices.clear();
        self.matched_scores.clear();

        if self.query.is_empty() {
            // Empty query = full history newest-first, no filtering, no indices
            let mut seen = std::collections::HashSet::new();
            for entry in history.iter().rev() {
                if seen.insert(entry.clone()) {
                    self.matches.push(entry.clone());
                    self.matched_indices.push(Vec::new());
                    self.matched_scores.push(0);
                }
            }
        } else {
            let query_lower = self.query.to_lowercase();
            let query_lower_str: &str = &query_lower;

            // Collect candidates with (text, is_substring, fuzzy_score, indices, recency_index)
            let mut candidates: Vec<(String, bool, u32, Vec<usize>, usize)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut recency = 0usize;

            for entry in history.iter().rev() {
                if !seen.insert(entry.clone()) {
                    continue;
                }
                recency += 1;

                let entry_lower = entry.to_lowercase();
                if entry_lower.contains(query_lower_str) {
                    // Substring tier: find the char indices of the query occurrence
                    let start = entry_lower.find(query_lower_str).unwrap_or(0);
                    let indices: Vec<usize> =
                        (start..start + query_lower.chars().count()).collect();
                    candidates.push((entry.clone(), true, SUBSTRING_BASE, indices, recency));
                } else {
                    // Fuzzy tier: nucleo-matcher
                    let mut idx_buf = Vec::new();
                    let haystack = Utf32Str::new(entry, &mut idx_buf);
                    let mut needle_buf: Vec<char> = Vec::new();
                    let needle = Utf32Str::new(query_lower_str, &mut needle_buf);
                    let mut indices_buf = Vec::new();
                    if let Some(score) =
                        self.fuzzy_matcher
                            .fuzzy_indices(haystack, needle, &mut indices_buf)
                    {
                        let indices: Vec<usize> = indices_buf.iter().map(|&i| i as usize).collect();
                        candidates.push((entry.clone(), false, score as u32, indices, recency));
                    }
                }
            }

            // Sort: score DESC, then recency ASC (newer first on equal score)
            candidates.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.4.cmp(&b.4)));

            for (text, _is_sub, _score, indices, _recency) in candidates {
                self.matched_scores.push(0); // scores not exposed; sort already done
                self.matched_indices.push(indices);
                self.matches.push(text);
            }
        }
        self.match_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_history() -> Vec<String> {
        vec![
            "first".into(),
            "second".into(),
            "hello world".into(),
            "third".into(),
            "second".into(), // duplicate
            "fourth".into(),
        ]
    }

    // === Existing tests (updated for new fields) ===

    #[test]
    fn enter_snapshots_draft_and_clears() {
        let mut hs = HistorySearch::new();
        hs.enter("my draft");
        assert!(hs.active);
        assert_eq!(hs.draft_snapshot, "my draft");
        assert!(hs.query.is_empty());
        assert!(hs.matches.is_empty());
        assert!(hs.matched_indices.is_empty());
    }

    #[test]
    fn exit_resets_active() {
        let mut hs = HistorySearch::new();
        hs.enter("draft");
        hs.exit();
        assert!(!hs.active);
    }

    #[test]
    fn empty_query_shows_full_history_newest_first() {
        let history = sample_history();
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        // Newest-first, deduped: fourth, second, third, hello world, first
        assert_eq!(hs.matches.len(), 5);
        assert_eq!(hs.matches[0], "fourth");
        assert_eq!(hs.matches[1], "second"); // first occurrence from end
        assert_eq!(hs.matches[2], "third");
        assert_eq!(hs.matches[3], "hello world");
        assert_eq!(hs.matches[4], "first");
        // Empty query → no highlight indices
        for indices in &hs.matched_indices {
            assert!(indices.is_empty());
        }
    }

    #[test]
    fn substring_match_case_insensitive() {
        let history = sample_history();
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.push_char('S', &history);
        // second (newest, deduped) + first — "third" has no s, "hello world" has no s
        assert_eq!(hs.matches.len(), 2);
        assert!(hs.matches.iter().all(|m| m.to_lowercase().contains('s')));
    }

    #[test]
    fn dedup_identical_texts() {
        let history = vec!["abc".into(), "xyz".into(), "abc".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        // abc appears twice but should be deduped
        assert_eq!(hs.matches.len(), 2);
        assert_eq!(hs.matches[0], "abc");
        assert_eq!(hs.matches[1], "xyz");
    }

    #[test]
    fn cycle_older_clamps() {
        let history = vec!["a".into(), "b".into(), "c".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        assert_eq!(hs.match_index, 0);
        hs.cycle_older();
        assert_eq!(hs.match_index, 1);
        hs.cycle_older();
        assert_eq!(hs.match_index, 2);
        hs.cycle_older(); // clamp
        assert_eq!(hs.match_index, 2);
    }

    #[test]
    fn cycle_newer_clamps() {
        let history = vec!["a".into(), "b".into(), "c".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        hs.cycle_older();
        hs.cycle_older();
        assert_eq!(hs.match_index, 2);
        hs.cycle_newer();
        assert_eq!(hs.match_index, 1);
        hs.cycle_newer();
        assert_eq!(hs.match_index, 0);
        hs.cycle_newer(); // clamp
        assert_eq!(hs.match_index, 0);
    }

    #[test]
    fn current_match_returns_active_entry() {
        let history = vec!["alpha".into(), "beta".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        assert_eq!(hs.current_match(), Some("beta")); // newest first
        hs.cycle_older();
        assert_eq!(hs.current_match(), Some("alpha"));
    }

    #[test]
    fn backspace_removes_char_and_recomputes() {
        let history = vec!["hello".into(), "world".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.push_char('h', &history);
        assert_eq!(hs.matches.len(), 1);
        assert_eq!(hs.matches[0], "hello");
        hs.backspace(&history);
        assert!(hs.query.is_empty());
        assert_eq!(hs.matches.len(), 2); // full history again
    }

    #[test]
    fn no_matches_yields_empty() {
        let history = vec!["hello".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.push_char('z', &history);
        assert!(hs.matches.is_empty());
        assert_eq!(hs.current_match(), None);
    }

    // === New fuzzy matching tests ===

    #[test]
    fn fuzzy_match_transposed_chars() {
        // "wkl" should fuzzy-match "zen wiki list" (w-i-k-i contains w..k..l)
        let history = vec![
            "zen wiki list".into(),
            "cargo build".into(),
            "git status".into(),
        ];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        // "wkl" fuzzy matches "zen wiki list"
        hs.query = "wkl".into();
        hs.recompute_matches(&history);
        assert!(
            hs.matches.iter().any(|m| m == "zen wiki list"),
            "fuzzy 'wkl' must match 'zen wiki list': {:?}",
            hs.matches
        );
    }

    #[test]
    fn fuzzy_match_missing_chars() {
        // "hstry" should fuzzy-match entries like "hello world history test"
        let history = vec!["hello world history test".into(), "cargo build".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "hstry".into();
        hs.recompute_matches(&history);
        assert!(
            hs.matches.iter().any(|m| m.contains("history")),
            "fuzzy 'hstry' must match 'hello world history test': {:?}",
            hs.matches
        );
    }

    #[test]
    fn substring_outranks_fuzzy() {
        // "wiki" substring-matches "zen wiki list" (score=1_000_000+1)
        // "wiki" fuzzy-matches "walkie talkie" (score = algo u16, e.g. ~200)
        // Substring must rank first
        let history = vec![
            "walkie talkie".into(), // fuzzy match
            "zen wiki list".into(), // substring match
        ];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        // Both should match
        assert_eq!(hs.matches.len(), 2);
        // Substring match ("zen wiki list") must be first
        assert_eq!(
            hs.matches[0], "zen wiki list",
            "substring match must outrank fuzzy: {:?}",
            hs.matches
        );
    }

    #[test]
    fn equal_scores_newer_first() {
        // Two entries both substring-match with same query length → newer first
        let history = vec!["older entry alpha".into(), "newer entry alpha".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        hs.query = "alpha".into();
        hs.recompute_matches(&history);
        assert_eq!(hs.matches.len(), 2);
        // "newer entry alpha" is newer (history iter rev), should be first
        assert_eq!(hs.matches[0], "newer entry alpha");
    }

    #[test]
    fn indices_match_query_chars_in_order() {
        // "wkl" fuzzy-matches "zen wiki list"; indices should correspond to w, k, l
        let history = vec!["zen wiki list".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "wkl".into();
        hs.recompute_matches(&history);
        assert_eq!(hs.matches.len(), 1);
        let indices = hs.current_match_indices();
        assert!(
            !indices.is_empty(),
            "fuzzy match must produce highlight indices"
        );
        // Verify indices are in ascending order (char positions)
        for pair in indices.windows(2) {
            assert!(pair[0] < pair[1], "indices must be ascending: {indices:?}");
        }
        // Verify the chars at those indices spell out the query (lowercased)
        let candidate_lower = hs.matches[0].to_lowercase();
        let matched_chars: String = indices
            .iter()
            .filter_map(|&i| candidate_lower.chars().nth(i))
            .collect();
        assert!(
            matched_chars.contains("w") && matched_chars.contains("k"),
            "matched chars should include query chars: got '{matched_chars}'"
        );
    }

    #[test]
    fn substring_indices_contiguous() {
        // "list" substring-matches "zen wiki list" → indices should be contiguous
        let history = vec!["zen wiki list".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "list".into();
        hs.recompute_matches(&history);
        assert_eq!(hs.matches.len(), 1);
        let indices = hs.current_match_indices();
        assert!(!indices.is_empty(), "substring match must produce indices");
        // Contiguous: each index = previous + 1
        for pair in indices.windows(2) {
            assert_eq!(
                pair[1],
                pair[0] + 1,
                "substring indices must be contiguous: {indices:?}"
            );
        }
    }

    #[test]
    fn current_match_indices_empty_for_no_match() {
        let history = vec!["hello".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "zzz".into();
        hs.recompute_matches(&history);
        assert!(hs.matches.is_empty());
        assert!(hs.current_match_indices().is_empty());
    }

    #[test]
    fn current_match_indices_empty_for_empty_query() {
        let history = vec!["hello".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.recompute_matches(&history);
        assert_eq!(hs.matches.len(), 1);
        assert!(hs.current_match_indices().is_empty());
    }
}
