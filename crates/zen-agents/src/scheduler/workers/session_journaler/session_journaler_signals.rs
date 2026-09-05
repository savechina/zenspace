//! Signal-extraction helpers for the session journaler.
//!
//! Owns: LLM/keyword signal extraction, preference derivation,
//! quality pre-filtering, typed-signal routing, prompt context loading,
//! journal entry building, anti-pattern matching, and conversation text
//! assembly. Extracted from `session_journaler.rs` to keep the worker thin.

use std::fs;

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{debug, warn};

use zen_core::paths::ZenPaths;
use zen_core::sanitize::InputSanitizer;
use zen_core::types::Sensitivity;
use zen_memory::belief::Belief;
use zen_memory::correction::Correction;
use zen_memory::decision::{CostBreakdown, Decision};
use zen_memory::dream::{ExtractedSignals, extract_durable_facts_from_entry};
use zen_memory::feedback_signal::Feedback;
use zen_memory::preference::{PREFERENCE_PRIOR, Preference, PreferencePredicate};
use zen_memory::quality_gate::{
    DECISION_PRINCIPLES, EXTRACTION_GUARDRAILS, MemoryGrade, grade_session_signal,
};
use zen_provider::{DefaultRouter, LlmRouterExt};
use zen_vault::wiki::AtomicWikiWriter;

// ─── Types ─────────────────────────────────────────────────────────────

pub(crate) struct PromptContext {
    pub(crate) commitments_section: String,
    pub(crate) beliefs_section: String,
    pub(crate) anti_patterns_section: String,
}

impl PromptContext {
    pub(crate) fn is_empty(&self) -> bool {
        self.commitments_section.is_empty()
            && self.beliefs_section.is_empty()
            && self.anti_patterns_section.is_empty()
    }

    pub(crate) fn to_prompt_section(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n--- Context ---\n");
        if !self.commitments_section.is_empty() {
            s.push_str(&self.commitments_section);
            s.push('\n');
        }
        if !self.beliefs_section.is_empty() {
            s.push_str(&self.beliefs_section);
            s.push('\n');
        }
        if !self.anti_patterns_section.is_empty() {
            s.push_str(&self.anti_patterns_section);
            s.push('\n');
        }
        s
    }
}

pub(crate) struct CommitmentSummary {
    pub(crate) text: String,
    pub(crate) status: String,
    pub(crate) review_at: String,
}

// ─── Constants ─────────────────────────────────────────────────────────

/// Max words captured as a preference object — keeps heuristic triples
/// specific without parsing full sentences.
const PREFERENCE_MAX_OBJECT_WORDS: usize = 6;

/// Reflection fragments that mark stop-doing / anti-pattern content; matched
/// against the lowercased signal text.
const STOP_DOING_KEYWORDS: &[&str] = &[
    "stop",
    "avoid",
    "should not",
    "不要再",
    "停止",
    "别再",
    "戒掉",
];

// ─── Conversation text & journal entry building ────────────────────────

pub(crate) fn build_conversation_text(turns: &[(String, String)]) -> String {
    let mut text = String::new();
    for (role, content) in turns {
        text.push_str(&format!("{role}: {content}\n"));
    }
    text
}

pub(crate) fn build_journal_entry(
    session_id: &str,
    turn_count: usize,
    signals: &ExtractedSignals,
    source: &str,
) -> String {
    let now = Utc::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let timestamp_str = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut entry = format!(
        "---\nsession_id: {session_id}\ndate: {date_str}\nturn_count: {turn_count}\nsource: {source}\n---\n\n# Session Journal — {timestamp_str}\n\n"
    );

    entry.push_str("## Facts\n\n");
    if signals.facts.is_empty() {
        entry.push_str("_(no durable facts extracted)_\n\n");
    } else {
        for fact in &signals.facts {
            entry.push_str(&format!("- {fact}\n"));
        }
        entry.push('\n');
    }

    entry.push_str("## Reflections\n\n");
    if signals.reflections.is_empty() {
        entry.push_str("_(no reflections extracted)_\n\n");
    } else {
        for refl in &signals.reflections {
            entry.push_str(&format!("- {refl}\n"));
        }
        entry.push('\n');
    }

    entry.push_str("## Commitments\n\n");
    if signals.commitments.is_empty() {
        entry.push_str("_(no commitments extracted)_\n");
    } else {
        for comm in &signals.commitments {
            entry.push_str(&format!("- {comm}\n"));
        }
    }

    entry
}

// ─── Anti-pattern matching ─────────────────────────────────────────────

pub(crate) fn check_anti_pattern_match(
    session_text: &str,
    anti_patterns_dir: &std::path::Path,
) -> Vec<String> {
    let mut matched = Vec::new();
    if !anti_patterns_dir.is_dir() {
        return matched;
    }

    let entries = match fs::read_dir(anti_patterns_dir) {
        Ok(e) => e,
        Err(_) => return matched,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let id = parse_frontmatter_field(&content, "id").unwrap_or_default();
        if id.is_empty() {
            continue;
        }

        let trigger = parse_frontmatter_field(&content, "trigger").unwrap_or_default();
        if trigger.is_empty() {
            continue;
        }

        let trigger_lower = trigger.to_lowercase();
        let session_lower = session_text.to_lowercase();

        let keywords: Vec<&str> = trigger_lower
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .filter(|w| w.len() > 3)
            .collect();

        let match_count = keywords
            .iter()
            .filter(|kw| session_lower.contains(*kw))
            .count();
        let threshold = (keywords.len() / 2).max(1);

        if match_count >= threshold {
            matched.push(id);
        }
    }

    matched
}

// ─── Frontmatter parsing ───────────────────────────────────────────────

pub(crate) fn parse_frontmatter_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in content.lines().take(15) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

// ─── Prompt context loading ────────────────────────────────────────────

pub(crate) async fn load_prompt_context(paths: &ZenPaths) -> PromptContext {
    let commitments_section = load_top_commitments(paths, 5);
    let beliefs_section = load_top_beliefs(paths, 5);
    let anti_patterns_section = load_top_anti_patterns(paths, 5);
    PromptContext {
        commitments_section,
        beliefs_section,
        anti_patterns_section,
    }
}

fn load_top_commitments(paths: &ZenPaths, n: usize) -> String {
    let dir = paths.vault().join("memories/commitments");
    let items = scan_commitments(&dir);
    if items.is_empty() {
        return String::new();
    }
    let top: Vec<&CommitmentSummary> = items.iter().take(n).collect();
    let mut s = String::from("User's active commitments (prioritize signals relevant to these):\n");
    for item in top {
        s.push_str(&format!(
            "- {} [{}, review: {}]\n",
            item.text, item.status, item.review_at
        ));
    }
    s
}

fn load_top_beliefs(paths: &ZenPaths, n: usize) -> String {
    let dir = paths.vault().join("wiki/wisdom/beliefs");
    let beliefs = match zen_memory::belief::Belief::load_all(&dir) {
        Ok(b) => b,
        Err(_) => return String::new(),
    };
    if beliefs.is_empty() {
        return String::new();
    }
    let top = zen_memory::belief::top_by_priority(&beliefs, n);
    let mut s = String::from("User's current beliefs (by confidence):\n");
    for b in top {
        s.push_str(&format!(
            "- {} ({:.0}% confident)\n",
            b.proposition,
            b.posterior * 100.0
        ));
    }
    s
}

fn load_top_anti_patterns(paths: &ZenPaths, n: usize) -> String {
    let dir = paths.vault().join("wiki/wisdom/anti-patterns");
    let signals = match zen_memory::AntiPatternSignal::load_all(&dir) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if signals.is_empty() {
        return String::new();
    }
    let top = signals.iter().take(n);
    let mut s = String::from("Known anti-patterns to watch for during extraction:\n");
    for ap in top {
        s.push_str(&format!("- {} (trigger: {})\n", ap.pattern, ap.trigger));
    }
    s
}

fn scan_commitments(dir: &std::path::Path) -> Vec<CommitmentSummary> {
    let mut items = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return items,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let text = parse_frontmatter_field(&content, "text").unwrap_or_default();
        let status =
            parse_frontmatter_field(&content, "status").unwrap_or_else(|| "open".to_string());
        let review_at = parse_frontmatter_field(&content, "review_at").unwrap_or_default();
        if status == "open" && !text.is_empty() {
            items.push(CommitmentSummary {
                text,
                status,
                review_at,
            });
        }
    }
    items.sort_by(|a, b| a.review_at.cmp(&b.review_at));
    items
}

// ─── LLM signal extraction ────────────────────────────────────────────

pub(crate) async fn extract_signals_via_llm(
    conversation_text: &str,
    prompt_context: &PromptContext,
    router: DefaultRouter,
    matched_anti_patterns: &[String],
    fresh_eyes: bool,
) -> Result<ExtractedSignals> {
    let truncated = if conversation_text.len() > 12000 {
        let end = conversation_text
            .char_indices()
            .nth(12000)
            .map(|(i, _)| i)
            .unwrap_or(conversation_text.len());
        format!("{}...", &conversation_text[..end])
    } else {
        conversation_text.to_string()
    };

    let context_section = prompt_context.to_prompt_section();

    let anti_pattern_warning = if matched_anti_patterns.is_empty() {
        String::new()
    } else {
        format!(
            "\nWARNING: Detected anti-patterns: {}. Force reflection extraction for these patterns.\n",
            matched_anti_patterns.join(", ")
        )
    };

    let fresh_eyes_note = if fresh_eyes {
        "\n[FRESH EYES MODE] No prior context injected. Extract signals from conversation only.\n"
    } else {
        ""
    };

    let sanitizer = InputSanitizer::new();
    let truncated = sanitizer.strip_dangerous_patterns(&truncated);

    let prompt = format!(
        r#"Extract typed signals from this development session conversation.

Conversation:
{truncated}
{context_section}
{anti_pattern_warning}
{fresh_eyes_note}
{EXTRACTION_GUARDRAILS}
{DECISION_PRINCIPLES}
Respond with ONLY a JSON object:
{{
  "facts": [
    "Implemented JWT authentication with refresh token rotation",
    "Decided to use SQLite for local storage instead of PostgreSQL"
  ],
  "reflections": [
    "The login flow is too complex — users get confused at step 3",
    "Should have tested the migration on a copy first"
  ],
  "commitments": [
    "Simplify login to 2 steps by 2026-07-01",
    "Write integration tests for the auth module this week"
  ],
  "decisions": [
    {{"text": "Use SQLite over PostgreSQL for local-first storage", "context": "Need offline capability with minimal setup", "expected_value": "Lower ops cost, good enough for single-user"}}
  ],
  "corrections": [
    {{"error": "Assumed all env vars were set in production", "correct_answer": "Validate env vars at startup and fail fast", "cost": "2h debugging deploy failure"}}
  ],
  "feedback": [
    {{"target": "login-flow", "content": "Users abandon at step 3 of registration", "sentiment": "negative"}}
  ],
  "beliefs": [
    {{"statement": "SQLite is sufficient for local-first apps under 1GB data", "confidence": 0.7}}
  ],
  "continue_doing": [
    "Writing tests before implementation caught 3 regressions early",
    "Pair programming on the API design reduced rework by half"
  ]
}}

Rules:
- **Facts**: past-tense, specific, durable — useful after 6 months. Technical decisions, bug fixes, learnings.
- **Reflections**: what went wrong, what could be better, what surprised you. Self-critical, honest.
- **Commitments**: what you (the user) plan to do next. Include a rough timeframe if mentioned.
- **Decisions**: explicit choices between alternatives. Include context and expected value rationale.
- **Corrections**: errors caught and fixed. Include what went wrong, the correct answer, and the cost.
- **Feedback**: observations about code/process quality. Include target, content, and sentiment.
- **Beliefs**: assumptions or opinions held by the user. Include a confidence score (0.0-1.0).
- **Continue-doing**: positive actions or practices that worked well and should be repeated. Capture what went RIGHT — techniques, habits, or decisions that produced good outcomes. These are the "continue-doing" counterpart to reflections (stop-doing).
- Do NOT include transient mechanics ("user asked about X", "assistant replied")
- If a category is empty, return an empty array for it
- If nothing of value happened in any category, return all empty arrays"#
    );

    let response = tokio::task::spawn_blocking(move || {
        router.complete("signal_extraction", &prompt, Sensitivity::Private)
    })
    .await
    .context("LLM signal extraction task panicked")??;

    let json_str = if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") {
            &after[..end]
        } else {
            after
        }
    } else if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            &response[start..=end]
        } else {
            &response
        }
    } else {
        &response
    };

    let parsed: serde_json::Value = serde_json::from_str(json_str.trim())
        .context("failed to parse LLM signal extraction response")?;

    let mut signals = ExtractedSignals::default();

    if let Some(arr) = parsed["facts"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() && s != "No durable facts extracted." {
                    signals.facts.push(s.to_string());
                }
            }
        }
    }

    if let Some(arr) = parsed["reflections"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    signals.reflections.push(s.to_string());
                }
            }
        }
    }

    if let Some(arr) = parsed["commitments"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    signals.commitments.push(s.to_string());
                }
            }
        }
    }

    if let Some(arr) = parsed["decisions"].as_array() {
        for item in arr {
            if let Some(obj) = item.as_object()
                && let Some(text) = obj.get("text").and_then(|v| v.as_str())
            {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    let context = obj
                        .get("context")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let ev = obj
                        .get("expected_value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    signals.decisions.push(format!("{text}|||{context}|||{ev}"));
                }
            }
        }
    }

    if let Some(arr) = parsed["corrections"].as_array() {
        for item in arr {
            if let Some(obj) = item.as_object()
                && let Some(error) = obj.get("error").and_then(|v| v.as_str())
            {
                let error = error.trim().to_string();
                if !error.is_empty() {
                    let correct = obj
                        .get("correct_answer")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cost = obj
                        .get("cost")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    signals
                        .corrections
                        .push(format!("{error}|||{correct}|||{cost}"));
                }
            }
        }
    }

    if let Some(arr) = parsed["feedback"].as_array() {
        for item in arr {
            if let Some(obj) = item.as_object()
                && let Some(content) = obj.get("content").and_then(|v| v.as_str())
            {
                let content = content.trim().to_string();
                if !content.is_empty() {
                    let target = obj
                        .get("target")
                        .and_then(|v| v.as_str())
                        .unwrap_or("session")
                        .to_string();
                    let sentiment = obj
                        .get("sentiment")
                        .and_then(|v| v.as_str())
                        .unwrap_or("neutral")
                        .to_string();
                    signals
                        .feedback
                        .push(format!("{target}|||{content}|||{sentiment}"));
                }
            }
        }
    }

    if let Some(arr) = parsed["beliefs"].as_array() {
        for item in arr {
            if let Some(obj) = item.as_object()
                && let Some(statement) = obj.get("statement").and_then(|v| v.as_str())
            {
                let statement = statement.trim().to_string();
                if !statement.is_empty() {
                    let confidence = obj
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.5);
                    signals.beliefs.push(format!("{statement}|||{confidence}"));
                }
            }
        }
    }

    if let Some(arr) = parsed["continue_doing"].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    signals.continue_doing_candidates.push(s.to_string());
                }
            }
        }
    }

    Ok(signals)
}

// ─── Keyword signal extraction (fallback) ──────────────────────────────

pub(crate) fn extract_signals_via_keyword(conversation_text: &str) -> ExtractedSignals {
    let facts = extract_durable_facts_from_entry(conversation_text);
    ExtractedSignals {
        facts,
        reflections: Vec::new(),
        commitments: Vec::new(),
        decisions: Vec::new(),
        corrections: Vec::new(),
        feedback: Vec::new(),
        beliefs: Vec::new(),
        continue_doing_candidates: Vec::new(),
        preferences: Vec::new(),
    }
}

// ─── T070: preference derivation (FR-021 Pi point 2) ────────────────────

/// Derive `Preference` triples from raw conversation text (heuristic, no LLM).
///
/// Scans each line for a preference predicate keyword (`like`/`likes`/`喜欢`,
/// `prefer`/`prefers`/`偏好`) and captures the trailing phrase as the object
/// (up to [`PREFERENCE_MAX_OBJECT_WORDS`] words). Subject is always `user` —
/// session conversations are the user's own voice. Returns the documented
/// `subject|||predicate|||object|||confidence` serialization
/// (`ExtractedSignals::preferences`), confidence = FR-025 prior. Deduped,
/// order-preserving.
pub(crate) fn derive_preferences(conversation_text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in conversation_text.lines() {
        let lower = line.to_lowercase();
        let Some((object_start, predicate)) = find_preference_keyword(&lower) else {
            continue;
        };
        let object = take_words(&lower[object_start..], PREFERENCE_MAX_OBJECT_WORDS);
        if object.is_empty() {
            continue;
        }
        let preference = Preference::new("user", predicate, object);
        let serialized = format!(
            "{}|||{}|||{}|||{:.2}",
            preference.subject, preference.predicate, preference.object, preference.confidence
        );
        if seen.insert(serialized.clone()) {
            out.push(serialized);
        }
    }
    out
}

/// Find the first preference predicate keyword in a lowercased line.
/// Returns the byte offset just past the keyword plus the parsed predicate.
/// English keywords are space-delimited so `likely`/`liked`/`delivery` never
/// false-positive.
fn find_preference_keyword(lower: &str) -> Option<(usize, PreferencePredicate)> {
    for kw in [" likes ", " like ", "喜欢"] {
        if let Some(pos) = lower.find(kw) {
            return Some((pos + kw.len(), PreferencePredicate::Likes));
        }
    }
    for kw in [" prefers ", " prefer ", "偏好"] {
        if let Some(pos) = lower.find(kw) {
            return Some((pos + kw.len(), PreferencePredicate::Prefers));
        }
    }
    None
}

fn take_words(text: &str, max_words: usize) -> String {
    let trimmed = text.trim_end_matches(['.', ',', '!', '?', ';', '。', '，', '！', '？', '；']);
    trimmed
        .split_whitespace()
        .take(max_words)
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── T073: M2 journal pre-filter ────────────────────────────────────────

/// M2 journal pre-filter (T073): grade each journal-bound signal via
/// `grade_session_signal` BEFORE the journal write.
///
/// - `Unverified` facts/commitments are dropped (debug log with fail_reasons).
/// - `Unverified` reflections containing stop-doing/anti-pattern content are
///   NOT silently dropped: they route to `wiki/wisdom/anti-patterns/` as
///   reduced-weight candidates (heuristic keyword match only — empty
///   trigger/avoidance, `session-journaler` provenance; downstream
///   synthesis corroborates or discards them), then leave the journal.
///
/// Returns the filtered signals and the number of dropped entries.
pub(crate) fn apply_quality_prefilter(
    vault: &std::path::Path,
    signals: ExtractedSignals,
    llm_extracted: bool,
) -> (ExtractedSignals, usize) {
    let mut filtered = 0usize;
    let mut out = signals;

    out.facts
        .retain(|s| keep_signal(s, llm_extracted, &mut filtered));
    out.commitments
        .retain(|s| keep_signal(s, llm_extracted, &mut filtered));

    let mut reflections = Vec::with_capacity(out.reflections.len());
    for text in out.reflections {
        let (grade, gate) = grade_session_signal(&text, llm_extracted);
        if grade == MemoryGrade::Unverified {
            if looks_like_stop_doing(&text) {
                route_stop_doing_candidate(vault, &text);
            } else {
                debug!(
                    signal = %text,
                    reasons = ?gate.fail_reasons(),
                    "quality pre-filter dropped signal"
                );
            }
            filtered += 1;
            continue;
        }
        reflections.push(text);
    }
    out.reflections = reflections;

    (out, filtered)
}

fn keep_signal(text: &str, llm_extracted: bool, filtered: &mut usize) -> bool {
    let (grade, gate) = grade_session_signal(text, llm_extracted);
    if grade == MemoryGrade::Unverified {
        debug!(
            signal = %text,
            reasons = ?gate.fail_reasons(),
            "quality pre-filter dropped signal"
        );
        *filtered += 1;
        return false;
    }
    true
}

fn looks_like_stop_doing(text: &str) -> bool {
    let lower = text.to_lowercase();
    STOP_DOING_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Route an Unverified stop-doing reflection to `wiki/wisdom/anti-patterns/`
/// at reduced weight: saved with empty trigger/avoidance and
/// `session-journaler` provenance (vs. wisdom_synth's evidence-backed
/// pages). Slug-dedup — an existing page for the same pattern stays untouched.
fn route_stop_doing_candidate(vault: &std::path::Path, text: &str) {
    let signal = zen_memory::AntiPatternSignal {
        pattern: text.to_string(),
        trigger: String::new(),
        avoidance: String::new(),
        detected_in: vec!["session-journaler".to_string()],
    };
    let dir = vault.join("wiki/wisdom/anti-patterns");
    if dir.join(format!("{}.md", signal.slug())).exists() {
        return;
    }
    match signal.save(&dir) {
        Ok(_) => debug!(
            pattern = %text,
            "unverified stop-doing signal routed to anti-patterns (reduced weight)"
        ),
        Err(e) => warn!(error = %e, pattern = %text, "failed to route stop-doing candidate"),
    }
}

// ─── Typed-signal routing / persistence ────────────────────────────────

pub(crate) fn save_typed_signals(paths: &ZenPaths, signals: &ExtractedSignals) {
    let vault = paths.vault();

    for raw in &signals.decisions {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        if parts.is_empty() {
            continue;
        }
        let text = parts[0].trim();
        let context = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let _expected_value = parts.get(2).map(|s| s.trim()).unwrap_or("");

        let id = Decision::slugify_title(text);
        let mut decision = Decision::new(id, text.to_string(), "session".to_string());
        decision.goal = context.to_string();
        if let Err(e) = decision.save(&vault.join("wiki/wisdom/decisions")) {
            warn!(error = %e, text = %text, "failed to save decision");
        }
    }

    for raw in &signals.corrections {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        if parts.is_empty() {
            continue;
        }
        let error_ref = parts[0].trim();
        let fix = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let _cost_str = parts.get(2).map(|s| s.trim()).unwrap_or("");

        let correction = Correction::new(error_ref, fix, CostBreakdown::default());
        if let Err(e) = correction.save(&vault.join("wiki/wisdom/corrections")) {
            warn!(error = %e, error_ref = %error_ref, "failed to save correction");
        }
    }

    for raw in &signals.feedback {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        if parts.is_empty() {
            continue;
        }
        let target = parts[0].trim();
        let content = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let _sentiment = parts.get(2).map(|s| s.trim()).unwrap_or("");

        let feedback = Feedback::new(target, content);
        if let Err(e) = feedback.save(&vault.join("wiki/wisdom/feedback")) {
            warn!(error = %e, target = %target, "failed to save feedback");
        }
    }

    for raw in &signals.beliefs {
        let parts: Vec<&str> = raw.splitn(2, "|||").collect();
        if parts.is_empty() {
            continue;
        }
        let statement = parts[0].trim();
        let confidence = parts
            .get(1)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.5);

        let id = zen_memory::belief::slugify_proposition(statement);
        let mut belief = Belief::new(id, statement.to_string(), "session".to_string());
        belief.posterior = confidence.clamp(0.01, 0.99);
        if let Err(e) = belief.save(&vault.join("wiki/wisdom/beliefs")) {
            warn!(error = %e, statement = %statement, "failed to save belief candidate");
        }
    }

    for raw in &signals.facts {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let fact = zen_memory::Fact::new(trimmed, "session", Vec::new());
        if let Err(e) = fact.save(&vault.join("wiki/wisdom/facts")) {
            warn!(error = %e, what = %trimmed, "failed to save fact");
        }
    }

    for raw in &signals.preferences {
        let parts: Vec<&str> = raw.splitn(4, "|||").collect();
        let Some(subject) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(predicate) = parts
            .get(1)
            .and_then(|s| PreferencePredicate::parse_predicate(s.trim()))
        else {
            continue;
        };
        let Some(object) = parts.get(2).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let confidence = parts
            .get(3)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(PREFERENCE_PRIOR);
        let preference = Preference::new(subject, predicate, object).with_confidence(confidence);

        let dir = vault.join("wiki/wisdom/preferences");
        let writer = AtomicWikiWriter::new(&dir);
        let page = format!("{}.md", preference.id());
        if let Err(e) = writer.write(std::path::Path::new(&page), &preference.to_markdown()) {
            warn!(error = %e, id = %preference.id(), "failed to save preference");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_journal_entry() {
        let session_id = "01JX0TEST000000000000000000";
        let turn_count = 5;
        let signals = ExtractedSignals {
            facts: vec![
                "completed auth module".to_string(),
                "fixed login bug".to_string(),
            ],
            reflections: vec![],
            commitments: vec![],
            decisions: vec![],
            corrections: vec![],
            feedback: vec![],
            beliefs: vec![],
            continue_doing_candidates: vec![],
            preferences: vec![],
        };

        let entry = build_journal_entry(session_id, turn_count, &signals, "keyword");

        assert!(entry.contains("session_id: 01JX0TEST000000000000000000"));
        assert!(entry.contains("turn_count: 5"));
        assert!(entry.contains("source: keyword"));
        assert!(!entry.contains("journaled_at:"));
        assert!(entry.contains("completed auth module"));
        assert!(entry.contains("fixed login bug"));
        assert!(entry.contains("## Facts"));
        assert!(entry.contains("## Reflections"));
        assert!(entry.contains("## Commitments"));
    }

    #[test]
    fn test_build_journal_entry_empty_signals() {
        let session_id = "01JX0TEST000000000000000000";
        let signals = ExtractedSignals::default();
        let entry = build_journal_entry(session_id, 3, &signals, "keyword");

        assert!(entry.contains("_(no durable facts extracted)_"));
        assert!(entry.contains("_(no reflections extracted)_"));
        assert!(entry.contains("_(no commitments extracted)_"));
    }

    #[test]
    fn test_build_journal_entry_all_sections() {
        let session_id = "01JX0TEST000000000000000000";
        let signals = ExtractedSignals {
            facts: vec!["implemented auth".to_string()],
            reflections: vec!["login flow too complex".to_string()],
            commitments: vec!["simplify login by July".to_string()],
            decisions: vec![],
            corrections: vec![],
            feedback: vec![],
            beliefs: vec![],
            continue_doing_candidates: vec![],
            preferences: vec![],
        };
        let entry = build_journal_entry(session_id, 10, &signals, "llm");

        assert!(entry.contains("## Facts"));
        assert!(entry.contains("- implemented auth"));
        assert!(entry.contains("## Reflections"));
        assert!(entry.contains("- login flow too complex"));
        assert!(entry.contains("## Commitments"));
        assert!(entry.contains("- simplify login by July"));
        assert!(entry.contains("source: llm"));
    }

    #[test]
    fn test_keyword_fallback_returns_facts_only() {
        let conversation = "user: completed the auth module\nassistant: great";
        let signals = extract_signals_via_keyword(conversation);

        assert!(!signals.facts.is_empty(), "keyword should extract facts");
        assert!(
            signals.reflections.is_empty(),
            "keyword returns no reflections"
        );
        assert!(
            signals.commitments.is_empty(),
            "keyword returns no commitments"
        );
    }

    #[test]
    fn test_build_conversation_text() {
        let turns = vec![
            ("user".to_string(), "Hello".to_string()),
            ("assistant".to_string(), "Hi there!".to_string()),
        ];

        let text = build_conversation_text(&turns);

        assert_eq!(text, "user: Hello\nassistant: Hi there!\n");
    }

    #[test]
    fn test_load_top_commitments_empty_dir() {
        let _dir = tempfile::tempdir().unwrap();
        let paths = ZenPaths::detect().unwrap_or_else(|_| {
            panic!("ZenPaths::detect failed");
        });
        let result = load_top_commitments(&paths, 5);
        assert!(result.is_empty() || result.contains("active commitments"));
    }

    #[test]
    fn test_load_top_beliefs_empty_dir() {
        let _dir = tempfile::tempdir().unwrap();
        let paths = ZenPaths::detect().unwrap_or_else(|_| {
            panic!("ZenPaths::detect failed");
        });
        let result = load_top_beliefs(&paths, 5);
        assert!(result.is_empty() || result.contains("beliefs"));
    }

    #[test]
    fn test_prompt_context_empty_to_prompt_section() {
        let ctx = PromptContext {
            commitments_section: String::new(),
            beliefs_section: String::new(),
            anti_patterns_section: String::new(),
        };
        assert!(ctx.is_empty());
        assert!(ctx.to_prompt_section().is_empty());
    }

    #[test]
    fn test_prompt_context_nonempty_has_sections() {
        let ctx = PromptContext {
            commitments_section: "commitments here\n".to_string(),
            beliefs_section: "beliefs here\n".to_string(),
            anti_patterns_section: String::new(),
        };
        assert!(!ctx.is_empty());
        let section = ctx.to_prompt_section();
        assert!(section.contains("--- Context ---"));
        assert!(section.contains("commitments here"));
        assert!(section.contains("beliefs here"));
    }

    #[test]
    fn test_parse_frontmatter_field_found() {
        let content = "---\ntext: Do the thing\nstatus: open\n---\n\nbody";
        assert_eq!(
            parse_frontmatter_field(content, "text"),
            Some("Do the thing".to_string())
        );
        assert_eq!(
            parse_frontmatter_field(content, "status"),
            Some("open".to_string())
        );
    }

    #[test]
    fn test_parse_frontmatter_field_missing() {
        let content = "---\ntext: Do the thing\n---\n\nbody";
        assert_eq!(parse_frontmatter_field(content, "status"), None);
    }

    #[test]
    fn test_scan_commitments_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let items = scan_commitments(dir.path());
        assert!(items.is_empty());
    }

    #[test]
    fn test_scan_commitments_filters_closed() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\ntext: Open task\nstatus: open\nreview_at: 2026-07-01T00:00:00Z\n---\n\n# Commitment\n\nOpen task\n";
        fs::write(dir.path().join("open.md"), content).unwrap();

        let closed = "---\ntext: Done task\nstatus: done\nreview_at: 2026-06-01T00:00:00Z\n---\n\n# Commitment\n\nDone task\n";
        fs::write(dir.path().join("closed.md"), closed).unwrap();

        let items = scan_commitments(dir.path());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "Open task");
    }

    #[test]
    fn test_check_anti_pattern_match_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let matched = check_anti_pattern_match("some session text", dir.path());
        assert!(matched.is_empty());
    }

    #[test]
    fn test_check_anti_pattern_match_nonexistent_dir() {
        let matched = check_anti_pattern_match("text", std::path::Path::new("/nonexistent"));
        assert!(matched.is_empty());
    }

    #[test]
    fn test_check_anti_pattern_match_with_trigger() {
        let dir = tempfile::tempdir().unwrap();
        let ap_content = "---\nid: confirmation-bias\ntype: anti-pattern\ntrigger: \"Selectively gathering evidence that supports existing beliefs\"\nseverity: high\n---\n\n# Confirmation Bias\n\nBody\n";
        fs::write(dir.path().join("confirmation-bias.md"), ap_content).unwrap();

        let session =
            "I only looked for evidence that supports my existing beliefs about the architecture";
        let matched = check_anti_pattern_match(session, dir.path());
        assert!(matched.contains(&"confirmation-bias".to_string()));
    }

    #[test]
    fn test_check_anti_pattern_match_no_trigger_match() {
        let dir = tempfile::tempdir().unwrap();
        let ap_content = "---\nid: anchoring-effect\ntype: anti-pattern\ntrigger: \"First number or estimate disproportionately influencing judgment\"\nseverity: med\n---\n\n# Anchoring\n\nBody\n";
        fs::write(dir.path().join("anchoring-effect.md"), ap_content).unwrap();

        let session =
            "We discussed the project timeline and decided on a different approach entirely";
        let matched = check_anti_pattern_match(session, dir.path());
        assert!(matched.is_empty());
    }

    #[test]
    fn test_check_anti_pattern_match_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let ap1 = "---\nid: pattern-a\ntype: anti-pattern\ntrigger: \"selectively gathering evidence supporting beliefs\"\nseverity: high\n---\n\nBody\n";
        let ap2 = "---\nid: pattern-b\ntype: anti-pattern\ntrigger: \"generating face-saving excuses instead honest assessment\"\nseverity: high\n---\n\nBody\n";
        fs::write(dir.path().join("a.md"), ap1).unwrap();
        fs::write(dir.path().join("b.md"), ap2).unwrap();

        let session = "I was selectively gathering evidence supporting my beliefs and also generating face-saving excuses instead honest assessment";
        let matched = check_anti_pattern_match(session, dir.path());
        assert!(matched.len() >= 2);
    }

    #[test]
    fn test_derive_preferences_from_sample_conversation() {
        let conversation = "user: I like concise code with fast builds\n\
                            assistant: noted\n\
                            user: 我喜欢简洁的工具\n\
                            user: prefers vim over emacs here";
        let prefs = derive_preferences(conversation);

        assert_eq!(prefs.len(), 3, "prefs: {prefs:?}");
        assert!(
            prefs[0].starts_with("user|||likes|||concise code with fast"),
            "{}",
            prefs[0]
        );
        assert!(prefs[0].ends_with("|||0.50"), "{}", prefs[0]);
        assert_eq!(prefs[1], "user|||likes|||简洁的工具|||0.50");
        assert_eq!(prefs[2], "user|||prefers|||vim over emacs here|||0.50");
    }

    #[test]
    fn test_derive_preferences_dedup_and_non_matches() {
        let conversation = "user: I like rust\n\
                            user: I like rust\n\
                            assistant: we shipped the feature today\n\
                            user: the build will likely pass";
        let prefs = derive_preferences(conversation);

        assert_eq!(prefs.len(), 1, "dedup + no false positive: {prefs:?}");
        assert_eq!(prefs[0], "user|||likes|||rust|||0.50");
    }

    #[test]
    fn test_preference_signals_persisted_to_wiki() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        let signals = ExtractedSignals {
            preferences: vec![
                "user|||likes|||concise code|||0.50".to_string(),
                "user|||bogus|||x|||0.50".to_string(),
            ],
            ..ExtractedSignals::default()
        };

        save_typed_signals(&paths, &signals);

        let page = paths
            .vault()
            .join("wiki/wisdom/preferences/user-likes-concise-code.md");
        assert!(page.exists(), "expected {}", page.display());
        let content = fs::read_to_string(&page).unwrap();
        assert!(content.contains("type: preference"));
        assert!(content.contains("predicate: likes"));
        assert!(content.contains("confidence: 0.50"));
    }

    // ── T073: quality pre-filter ──

    #[test]
    fn test_unverified_signal_filtered_by_prefilter() {
        let dir = tempfile::tempdir().unwrap();
        let signals = ExtractedSignals {
            facts: vec![
                "ok".to_string(),
                "migrated the session store to sqlite wal mode".to_string(),
            ],
            commitments: vec!["do it".to_string()],
            ..ExtractedSignals::default()
        };

        let (filtered, dropped) = apply_quality_prefilter(dir.path(), signals, false);

        assert_eq!(dropped, 2);
        assert_eq!(filtered.facts.len(), 1);
        assert_eq!(
            filtered.facts[0],
            "migrated the session store to sqlite wal mode"
        );
        assert!(filtered.commitments.is_empty());
    }

    #[test]
    fn test_unverified_stop_doing_reflection_routed_to_anti_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let signals = ExtractedSignals {
            reflections: vec!["stop skipping tests".to_string()],
            ..ExtractedSignals::default()
        };

        let (filtered, dropped) = apply_quality_prefilter(dir.path(), signals, false);

        assert_eq!(dropped, 1);
        assert!(filtered.reflections.is_empty());

        let ap_dir = dir.path().join("wiki/wisdom/anti-patterns");
        let entries: Vec<_> = fs::read_dir(&ap_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "one reduced-weight candidate");
        let content = fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
        assert!(content.contains("session-journaler"));
        assert!(content.contains("stop skipping tests"));
    }

    #[test]
    fn test_unverified_non_stop_doing_reflection_dropped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let signals = ExtractedSignals {
            reflections: vec!["ok then".to_string()],
            ..ExtractedSignals::default()
        };

        let (filtered, dropped) = apply_quality_prefilter(dir.path(), signals, false);

        assert_eq!(dropped, 1);
        assert!(filtered.reflections.is_empty());
        assert!(!dir.path().join("wiki/wisdom/anti-patterns").exists());
    }
}
