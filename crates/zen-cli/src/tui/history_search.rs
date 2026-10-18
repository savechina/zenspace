//! W2+FUZZY: Reverse-i-search state for the TUI input prompts.
//!
//! Fuzzy matching via nucleo-matcher (fzf-v2 similarity algorithm, MPL-2.0):
//! case-insensitive substring matches rank above fuzzy-only matches,
//! newest-first deduped results, draft snapshot on enter, restore on cancel.
//!
//! The key handler ([`handle_key`]) is the SINGLE shared state machine called
//! by both the inline prompt (`inline_handler`) and the fullscreen TUI
//! (`handler`), so `Ctrl+R=search` behaves identically in both render paths.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::App;
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Substring matches always outrank fuzzy matches: substring score is a
/// constant `SUBSTRING_BASE`; within a tier, recency (newest first) breaks ties.
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
        self.draft_snapshot = current_text.to_string();
    }

    /// Exit search mode, resetting all state.
    pub fn exit(&mut self) {
        self.active = false;
        self.query.clear();
        self.matches.clear();
        self.match_index = 0;
        self.matched_indices.clear();
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
    /// - **Substring tier**: candidate contains query (case-insensitive) → score = SUBSTRING_BASE (constant); recency orders within the tier.
    ///   Substring matches always outrank fuzzy-only matches.
    /// - **Fuzzy tier**: nucleo-matcher fuzzy_match score (algorithmic, u16→u32).
    /// - No match → excluded.
    ///
    /// Sort: score DESC, recency (newer first) as tiebreak.
    pub(crate) fn recompute_matches(&mut self, history: &[String]) {
        self.matches.clear();
        self.matched_indices.clear();

        if self.query.is_empty() {
            // Empty query = full history newest-first, no filtering, no indices
            let mut seen = std::collections::HashSet::new();
            for entry in history.iter().rev() {
                if seen.insert(entry.clone()) {
                    self.matches.push(entry.clone());
                    self.matched_indices.push(Vec::new());
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
                    // `find` returns a BYTE offset; the renderer highlights
                    // by CHAR index — convert or CJK entries misalign.
                    let byte_start = entry_lower.find(query_lower_str).unwrap_or(0);
                    let char_start = entry_lower[..byte_start].chars().count();
                    let indices: Vec<usize> =
                        (char_start..char_start + self.query.chars().count()).collect();
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
                self.matched_indices.push(indices);
                self.matches.push(text);
            }
        }
        self.match_index = 0;
    }
}

/// Handle one key event for reverse-i-search — the SINGLE shared state
/// machine called by both the inline prompt (`inline_handler::handle_key`)
/// and the fullscreen TUI (`handler::handle_key`), so `Ctrl+R=search` behaves
/// identically in both render paths without duplicating the logic.
///
/// Two responsibilities, in order:
///
/// 1. **Active search** (`app.history_search.active`): route Ctrl+R/Up (cycle
///    older), Down (cycle newer), Enter (accept match; restore draft on no
///    match), Esc / Ctrl+C / Ctrl+D (cancel → restore draft), Backspace (edit
///    query; empty query exits) and printable characters (narrow the query) —
///    every other key is swallowed while the search owns input.
/// 2. **Entry point** (not active): a Ctrl+R press snapshots the current
///    composer text as the draft, recomputes matches over
///    `app.command_history`, and previews the newest match in `app.input`.
///    Suppressed while the slash popup is visible (the popup owns keys), but
///    still swallowed so the key never reaches the composer.
///
/// Parameters: `key` — the crossterm key event; `app` — the shared TUI app
/// (all search state lives on it: `history_search`, `command_history`,
/// `slash_state`, `input`).
///
/// Returns: `true` when the key was consumed by the search (either branch),
/// `false` when the caller must continue with normal dispatch.
///
/// Errors: none — every branch is an in-memory mutation of `App`.
///
/// Call sites (usage):
/// ```text
/// inline:    if history_search::handle_key(key, app) { return Continue; }
/// fullscreen: while active → route every key here; Ctrl+R → delegate from
///             the main match arm after the session-picker precedence check.
/// ```
pub fn handle_key(key: KeyEvent, app: &mut App) -> bool {
    if app.history_search.active {
        match (key.code, key.modifiers) {
            // Ctrl+R or Up: cycle to older match
            (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::Up, KeyModifiers::NONE) => {
                app.history_search.cycle_older();
                if let Some(m) = app.history_search.current_match() {
                    let text = m.to_string();
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(text);
                    app.input.enter_history_mode();
                }
            }
            // Down: cycle to newer match
            (KeyCode::Down, KeyModifiers::NONE) => {
                app.history_search.cycle_newer();
                if let Some(m) = app.history_search.current_match() {
                    let text = m.to_string();
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(text);
                    app.input.enter_history_mode();
                } else {
                    // No match — restore draft
                    let draft = app.history_search.draft_snapshot.clone();
                    // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                }
            }
            // Enter: ACCEPT — set textarea to match, exit search.
            // BUG-4: If no match, restore draft (same as Esc/cancel).
            (KeyCode::Enter, _) => {
                if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(m);
                    app.input.exit_mode();
                } else {
                    // No match — restore draft (bash semantics: failed search leaves line unchanged).
                    let draft = app.history_search.draft_snapshot.clone();
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                }
                app.history_search.exit();
                // Refresh slash state with the RESULTING textarea text.
                // T084(d): dismissal-aware refresh (pure token equality).
                app.refresh_slash_popup();
            }
            // Esc: CANCEL — restore draft, exit search
            (KeyCode::Esc, _) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Ctrl+C: exit search only (do not quit app)
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Ctrl+D: exit search (do not quit app)
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let draft = app.history_search.draft_snapshot.clone();
                app.history_search.exit();
                // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                app.input = App::create_input_textarea_at_end(draft);
                app.input.exit_mode();
            }
            // Backspace: remove char from query; empty query + Backspace → exit
            (KeyCode::Backspace, _) => {
                if app.history_search.query.is_empty() {
                    let draft = app.history_search.draft_snapshot.clone();
                    app.history_search.exit();
                    // BUG-2+L3: Use new_at_end to position cursor at end of draft.
                    app.input = App::create_input_textarea_at_end(draft);
                    app.input.exit_mode();
                } else {
                    app.history_search.backspace(&app.command_history);
                    if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                        // BUG-2+L3: Use new_at_end to position cursor at end of match.
                        app.input = App::create_input_textarea_at_end(m);
                        app.input.enter_history_mode();
                    } else {
                        // No matches — show query in textarea
                        let q = app.history_search.query.clone();
                        // BUG-2+L3: Use new_at_end to position cursor at end of query.
                        app.input = App::create_input_textarea_at_end(q);
                        app.input.enter_history_mode();
                    }
                }
            }
            // Printable char: push to query
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                app.history_search.push_char(c, &app.command_history);
                if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                    // BUG-2+L3: Use new_at_end to position cursor at end of match.
                    app.input = App::create_input_textarea_at_end(m);
                    app.input.enter_history_mode();
                } else {
                    let q = app.history_search.query.clone();
                    // BUG-2+L3: Use new_at_end to position cursor at end of query.
                    app.input = App::create_input_textarea_at_end(q);
                    app.input.enter_history_mode();
                }
            }
            // Everything else: swallow while search is active
            _ => {}
        }
        return true;
    }

    // W2 entry: Ctrl+R in normal context → start reverse-i-search.
    // L2: Guard against slash popup — swallow Ctrl+R while popup owns keys.
    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if !app.slash_state.visible {
            let current_text = app.input.lines().join("\n");
            app.history_search.enter(&current_text);
            app.history_search.recompute_matches(&app.command_history);
            // Preview the first match (or full history if no query yet)
            if let Some(m) = app.history_search.current_match().map(|s| s.to_string()) {
                // BUG-2+L3: Use new_at_end to position cursor at end of match.
                app.input = App::create_input_textarea_at_end(m);
                app.input.enter_history_mode();
            }
        }
        return true;
    }
    false
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
    fn substring_indices_are_char_positions_for_cjk_entries() {
        // Multibyte chars before the match: `find` byte offset (中文abc →
        // "abc" at byte 6) must surface as CHAR indices [2, 3, 4], not [6..8].
        let history = vec!["中文abc".into(), "你好zen世界".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "abc".into();
        hs.recompute_matches(&history);
        assert_eq!(hs.matches, vec!["中文abc".to_string()]);
        assert_eq!(hs.current_match_indices(), vec![2, 3, 4]);

        hs.query = "zen".into();
        hs.recompute_matches(&history);
        assert_eq!(hs.matches, vec!["你好zen世界".to_string()]);
        assert_eq!(hs.current_match_indices(), vec![2, 3, 4]);
    }

    #[test]
    fn substring_indices_cjk_highlight_lands_on_match() {
        // End-to-end invariant: the chars at the highlighted indices spell
        // the query (the property the renderer relies on).
        let history = vec!["知识库wiki笔记".into()];
        let mut hs = HistorySearch::new();
        hs.enter("");
        hs.query = "wiki".into();
        hs.recompute_matches(&history);
        let indices = hs.current_match_indices();
        let matched: String = indices
            .iter()
            .filter_map(|&i| hs.matches[0].chars().nth(i))
            .collect();
        assert_eq!(matched, "wiki");
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
