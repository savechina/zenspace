//! LLM Distill Stage (FR-003 / T046) — bounded LLM enrichment on top of the
//! deterministic heuristic extraction.
//!
//! ## Scope Logic
//!
//! **Functionality**: when a provider is configured (the programmatic
//! [`DistillationPipeline::with_llm_model`] override, or
//! `[agentic.loop] merge_llm_model`), each note gets one bounded LLM
//! extraction call producing structured JSON notions. The call routes through
//! `DefaultRouter::route_with_preferences` at `Sensitivity::Private`
//! (local-first) by default; `[agentic.loop] distill_allow_cloud = true`
//! (env `ZEN_LOOP_DISTILL_ALLOW_CLOUD`) switches the request to
//! `Sensitivity::Public` so a cloud provider named in `merge_llm_model` is
//! actually reachable — the T045 `allow_cloud` encoding: naming a cloud
//! provider alone never moves note content. The Anthropic route additionally
//! honors `[agentic.cache] breakpoints` (compile-hygiene ③): the call goes
//! through `AnthropicProvider::complete_cached` so repeated cycles share the
//! cached system prefix and the real usage (`cache_creation` /
//! `cache_read_input_tokens`) becomes the token accounting. The T046 merge
//! assist [`merge_section`] reuses the same routing to compose merge
//! sections. Any failure — unconfigured provider, unreachable, garbage reply
//! — is fail-open: the affected note simply stays on the heuristic-only base
//! the pipeline already produced.
//!
//! **User impact**: with a provider configured, notion extraction gains
//! LLM-quality enrichment merged dedup-by-name over the heuristic base;
//! without one (or on any LLM failure) behaviour is byte-identical to the
//! heuristic path — zero regression. Token consumption is bounded by
//! [`LoopBudget::consume_tokens`]: an estimate is reserved before each call
//! (over-budget short-circuits the remaining notes) and the cached-path real
//! usage reconciles the reservation upward.
//!
//! **Default behavior**: `model: None` → zero LLM calls, zero tokens, zero
//! enrichment (this stage returns ONLY LLM-parsed notions — the heuristic
//! base is the caller's).
//!
//! **Interaction**: `merge_llm_model` is a PROVIDER name (see
//! `LlmPreference::Provider`), not a `provider:model` pair — the model is the
//! provider's configured default. The cached path fires only when the
//! routing actually selects Anthropic AND
//! `[agentic.cache] breakpoints_or_default()` is true (default true).

use anyhow::Result;
use serde_json::Value;
use tracing::{debug, info, warn};

use super::super::types::LoopBudget;
use crate::note::Note;
use crate::notion::Notion as NotionType;
use crate::notion::notion::parse_kind;

use zen_core::config::{LlmPreference, ModelOptions};
use zen_core::types::Sensitivity;
use zen_provider::cache::{CacheSegments, CacheUsage};
use zen_provider::{
    DefaultRouter, LlmRouter, Provider, ProviderInstance, TaskRequirements, usage_to_cost_usd,
};

/// Reply budget the extraction prompt asks for; part of every reservation.
const REPLY_ESTIMATE_TOKENS: u32 = 384;
/// Input characters sent per note — beyond this the note is truncated (the
/// heuristic path sees the full text; the LLM path stays bounded).
const MAX_NOTE_CHARS: usize = 12_000;
/// Notions accepted per note — a bloated reply is cut, not error-fatal.
const MAX_NOTIONS_PER_NOTE: usize = 8;
const MAX_NAME_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 300;
const MAX_ALIASES: usize = 8;
const MAX_TOPICS: usize = 8;
/// Merge-assist bounds: the unique-line input and the composed output are
/// both truncated so one merge can never blow the budget.
const MERGE_INPUT_CHARS: usize = 6_000;
const MERGE_OUTPUT_CHARS: usize = 4_000;

/// Stable, cacheable extraction system prompt (compile-hygiene ③: the system
/// segment is the cached prefix — it must not drift between cycles).
const EXTRACTION_SYSTEM: &str = "You are a knowledge-base extraction engine. \
From the given note, extract the notable notions (entities, concepts, \
technologies, people, organizations, decisions). Reply with ONLY a JSON \
array — no prose, no code fence. Each element: \
{\"name\": string, \"kind\": one of function|class|module|concept|person|\
organization|event|product|technology|other, \"description\": one sentence, \
\"aliases\": [alternative names], \"topics\": [themes]}. Extract at most 8 \
notions. Never invent facts absent from the note.";

/// Stable, cacheable merge-assist system prompt.
const MERGE_SYSTEM: &str = "You are a wiki merge assistant. You receive a \
target page title, a source page name, and the source lines that are new to \
the target. Rewrite the new lines as one cohesive markdown section body \
(sub-headings allowed, NO top-level title), preserving facts verbatim where \
possible and dropping redundancy. Reply with ONLY the section body — no \
prose, no code fence.";

/// LLM-enhanced distillation stage (FR-003, T046).
///
/// Holds a [`LoopBudget`] for token accounting and an optional provider
/// name. The stage is an ENRICHMENT producer: it returns only the notions
/// successfully parsed from LLM replies; the caller merges them over its own
/// heuristic base. `model: None` → no calls, no tokens.
pub struct LlmDistillStage {
    pub budget: LoopBudget,
    /// Target provider name (e.g. `"ollama"`, `"anthropic"`). `None` disables LLM.
    pub model: Option<String>,
}

impl LlmDistillStage {
    /// Create a new stage with the given budget ceiling and optional provider.
    pub fn new(budget: LoopBudget, model: Option<String>) -> Self {
        Self { budget, model }
    }

    /// Distill notes into LLM-enrichment notions. Returns
    /// `(notions, consumed_tokens)` — the caller merges the notions
    /// dedup-by-name over its heuristic base and folds the tokens into the
    /// cycle budget.
    pub async fn distill_with_fallback(
        &mut self,
        notes: &[Note],
    ) -> Result<(Vec<NotionType>, u32, f64)> {
        let Some(model) = self.model.clone() else {
            debug!("LlmDistillStage: no model configured — zero enrichment");
            return Ok((Vec::new(), 0, 0.0));
        };

        info!(
            model = %model,
            notes_count = notes.len(),
            "LlmDistillStage: LLM enrichment enabled"
        );

        let mut notions = Vec::new();
        let mut consumed = 0u32;
        let mut cost_spent = 0.0f64;
        for note in notes {
            let tree_pages = zen_core::config::load_config()
                .map(|c| c.agentic.loop_cfg.tree_index_pages_or_default())
                .unwrap_or(20);
            let (calls, toc) =
                if super::super::tree_index::should_segment(&note.content, tree_pages) {
                    match super::super::tree_index::segment_document(&note.content) {
                        // ⑥ long-doc path: one bounded call per heading section,
                        // the ToC riding each prompt as context — removes the
                        // single-call truncation blindness.
                        Some(sections) if sections.len() > 1 => {
                            let toc = super::super::tree_index::render_toc(&sections);
                            (
                                sections.into_iter().map(|s| (s.title, s.body)).collect(),
                                Some(toc),
                            )
                        }
                        // No headings / single section → plain truncated call.
                        _ => (vec![(String::new(), note.content.clone())], None),
                    }
                } else {
                    (vec![(String::new(), note.content.clone())], None)
                };
            for (section_title, section_body) in calls {
                let estimate = estimate_tokens(&section_body);
                if !self.budget.consume_tokens(estimate) {
                    warn!(
                        consumed_tokens = self.budget.consumed_tokens,
                        max_tokens = self.budget.max_tokens,
                        "LlmDistillStage: budget exhausted — remaining notes stay heuristic-only"
                    );
                    return Ok((notions, consumed, cost_spent));
                }
                let mut note_tokens = estimate;
                let task = match &toc {
                    Some(toc) => format!(
                        "Note id: {id}\nDocument outline:\n{toc}\n---\nExtract the notable notions from the section '{title}' below. Reply with ONLY the JSON array.\n\n{body}",
                        id = note.id,
                        title = section_title,
                        body = truncate_chars(&section_body, MAX_NOTE_CHARS),
                    ),
                    None => note_prompt(note),
                };
                match call_llm(&model, EXTRACTION_SYSTEM, &task).await {
                    Ok((reply, usage_tokens, cost_usd)) => {
                        self.budget.charge_cost(cost_usd);
                        cost_spent += cost_usd;
                        if usage_tokens > estimate {
                            let delta = usage_tokens - estimate;
                            if self.budget.consume_tokens(delta) {
                                note_tokens = usage_tokens;
                            }
                        }
                        notions.extend(parse_llm_notions(&reply, &note.id, MAX_NOTIONS_PER_NOTE));
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            note_id = %note.id,
                            "LlmDistillStage: LLM call failed — note stays heuristic-only"
                        );
                    }
                }
                consumed = consumed.saturating_add(note_tokens);
            }
        }
        Ok((notions, consumed, cost_spent))
    }
}

/// One bounded LLM call. Returns `(reply, actual_tokens, cost_usd)`; on the
// plain path the actual equals the caller-visible estimate of the prompt
// actually sent, on the cached path it is the real billable usage from the
// provider echo. `cost_usd` bills the call against the routed provider's
// `[providers.<name>]` pricing (0.0 for local providers — see
// `usage_to_cost_usd`) so the distill spend reaches the cost cap.
///
/// Sensitivity encoding (T045 precedent): `distill_allow_cloud = false`
/// (default) requests `Sensitivity::Private` so the router's
/// `enforce_sensitivity` gate keeps note content local; `true` requests
/// `Sensitivity::Public` — the explicit operator opt-in that lets a cloud
/// provider named in `merge_llm_model` serve distill calls.
async fn call_llm(
    model: &str,
    system: &str,
    task: &str,
) -> std::result::Result<(String, u32, f64), String> {
    let config = zen_core::config::load_config().map_err(|e| format!("config load failed: {e}"))?;
    let allow_cloud = config.agentic.loop_cfg.distill_allow_cloud_or_default();
    let use_cache = config.agentic.cache.breakpoints_or_default();
    let sensitivity = if allow_cloud {
        Sensitivity::Public
    } else {
        Sensitivity::Private
    };
    let router = DefaultRouter::from_agentic(config);
    let requirements = TaskRequirements {
        max_tokens: Some(2048),
        sensitivity,
        preferred_model: None,
        budget_limit: None,
    };
    let prefs = [LlmPreference::Provider(model.to_string())];
    let provider = router
        .route_with_preferences(&requirements, &prefs)
        .map_err(|e| {
            if !allow_cloud {
                format!(
                    "{e} — note: '{model}' cannot serve distill while the cloud gate is closed; \
                 set [agentic.loop] distill_allow_cloud = true (env \
                 ZEN_LOOP_DISTILL_ALLOW_CLOUD=1) to route note content through it"
                )
            } else {
                e.to_string()
            }
        })?;

    // Pricing metadata for the routed provider — mirrors
    // `complete_metered` (router.rs) so both cost paths bill identically.
    let provider_name = match &provider {
        Provider::Unknown(name) => name.clone(),
        _ => provider.to_string(),
    };
    let model_name = router
        .provider_instance(&provider_name)
        .map(|instance| instance.model_name().to_string())
        .unwrap_or_else(|| model.to_string());
    let metadata = router.model_metadata(&provider_name, &model_name);

    // Compile-hygiene ③: the Anthropic route honors [agentic.cache]
    // breakpoints — the stable system prompt becomes the cached prefix and
    // the real usage echo drives token accounting.
    if use_cache
        && provider == Provider::Anthropic
        && let Some(ProviderInstance::Anthropic(anthropic)) = router.provider_instance("anthropic")
    {
        let segments = CacheSegments::build(system, &[task.to_string()], "", &[]);
        let completion = anthropic
            .complete_cached(&segments, task, &ModelOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let billable = billable_tokens(&completion.usage, estimate_tokens(task));
        let cost = cache_cost_usd(&completion.usage, &metadata, billable);
        return Ok((completion.text, billable, cost));
    }

    // Plain path on a blocking thread — the established spawn_blocking guard:
    // route()/call() are sync and OllamaProvider constructs a nested tokio
    // Runtime inside them, which panics on an async worker thread.
    let prompt_tokens = estimate_tokens(task);
    let task = task.to_string();
    let reply = tokio::task::spawn_blocking(move || router.call(provider, &task))
        .await
        .map_err(|e| format!("distill LLM task join failed: {e}"))?
        .map_err(|e| e.to_string())?;
    let usage = plain_cost_split(prompt_tokens);
    let cost = usage_to_cost_usd(&metadata, usage.0, usage.1);
    Ok((reply, prompt_tokens, cost))
}

/// Cached-path cost split: `cache_creation` is billed as input, a cache
/// READ is not re-billed. Mirrors `billable_tokens`' semantics so the
/// billable-token number and the USD number stay coherent; an all-None
/// (unparseable) echo never bills, falling back on the token estimate.
fn cache_cost_usd(
    usage: &CacheUsage,
    metadata: &zen_provider::ModelMetadata,
    fallback: u32,
) -> f64 {
    match (usage.input_tokens, usage.output_tokens) {
        (None, None) => {
            let (i, o) = plain_cost_split(fallback);
            usage_to_cost_usd(metadata, i, o)
        }
        _ => {
            let input_raw = usage.input_tokens.unwrap_or(0);
            let creation = usage.cache_creation_input_tokens.unwrap_or(0);
            let read = usage.cache_read_input_tokens.unwrap_or(0);
            let input = (input_raw as i64 + creation as i64 - read as i64).max(0) as u64;
            usage_to_cost_usd(metadata, input, usage.output_tokens.unwrap_or(0))
        }
    }
}

/// Plain-path cost split: the reservation is `input_estimate + reply_budget`
/// (`estimate_tokens`), so bill the reply share as output.
fn plain_cost_split(prompt_tokens: u32) -> (u64, u64) {
    (
        prompt_tokens.saturating_sub(REPLY_ESTIMATE_TOKENS) as u64,
        REPLY_ESTIMATE_TOKENS as u64,
    )
}

/// Anthropic cache-usage echo → billable tokens (compile-hygiene ③
/// acceptance: the `cache_creation_input_tokens` share is observable, and a
/// cache READ is not re-billed as input). All-None usage (provider gave no
/// echo) falls back to the caller's estimate; a parseable echo never
/// estimates.
fn billable_tokens(usage: &CacheUsage, fallback: u32) -> u32 {
    match (usage.input_tokens, usage.output_tokens) {
        (None, None) => fallback,
        (input, output) => {
            let output = output.unwrap_or(0) as u32;
            let creation = usage.cache_creation_input_tokens.unwrap_or(0) as u32;
            let input = input
                .unwrap_or(0)
                .saturating_sub(usage.cache_read_input_tokens.unwrap_or(0))
                as u32;
            output.saturating_add(creation).saturating_add(input).max(1)
        }
    }
}

/// Parse the LLM reply into validated enrichment notions. Anything
/// unparseable, unknown-kind, or unnamed is dropped — a hallucinated or
/// malformed reply must never reach the notion base. Returns at most `max`.
fn parse_llm_notions(raw: &str, note_id: &str, max: usize) -> Vec<NotionType> {
    let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
        warn!("LlmDistillStage: reply has no JSON array — zero enrichment (fail-open)");
        return Vec::new();
    };
    if start >= end {
        return Vec::new();
    }
    let items: Vec<Value> = match serde_json::from_str(&raw[start..=end]) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "LlmDistillStage: reply is not a JSON array — zero enrichment (fail-open)");
            return Vec::new();
        }
    };
    let mut notions = Vec::new();
    for item in items {
        if notions.len() >= max {
            break;
        }
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let Some(kind) = item
            .get("kind")
            .and_then(Value::as_str)
            .and_then(|k| parse_kind(&k.to_lowercase()))
        else {
            continue;
        };
        let mut notion = NotionType::new(truncate_chars(name, MAX_NAME_CHARS), kind, note_id);
        if let Some(desc) = item.get("description").and_then(Value::as_str) {
            notion.description = truncate_chars(desc.trim(), MAX_DESCRIPTION_CHARS).to_string();
        }
        if let Some(aliases) = item.get("aliases").and_then(Value::as_array) {
            notion.aliases = aliases
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(MAX_ALIASES)
                .map(String::from)
                .collect();
        }
        if let Some(topics) = item.get("topics").and_then(Value::as_array) {
            notion.topics = topics
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .take(MAX_TOPICS)
                .map(String::from)
                .collect();
        }
        notions.push(notion);
    }
    notions
}

/// Compose a merge section via the LLM (T046 merge assist). Returns
/// `Some((body, tokens, cost_usd))` — the caller falls back to the
/// deterministic raw append on `None` (3A: the cost rides out so merge
/// assist spend reaches the cost cap; previously even `_tokens` were
/// discarded). Input/output are truncated so one merge can never blow
/// the budget.
pub async fn merge_section(
    model: &str,
    target_title: &str,
    source_stem: &str,
    unique: &str,
) -> Option<(String, u32, f64)> {
    let unique = truncate_chars(unique, MERGE_INPUT_CHARS);
    let task = format!(
        "Target page: {target_title}\nSource page: {source_stem}\n\nNew lines to absorb:\n\n{unique}\n\nRewrite the new lines as one cohesive markdown section body (sub-headings allowed, no top-level title), preserving facts verbatim where possible. Reply with ONLY the section body."
    );
    match call_llm(model, MERGE_SYSTEM, &task).await {
        Ok((reply, tokens, cost)) => {
            let body = truncate_chars(reply.trim(), MERGE_OUTPUT_CHARS).to_string();
            if body.is_empty() {
                warn!("merge assist: empty LLM section — falling back to raw append");
                None
            } else {
                Some((body, tokens, cost))
            }
        }
        Err(e) => {
            warn!(error = %e, "merge assist: LLM call failed — falling back to raw append");
            None
        }
    }
}

/// The per-note user prompt: id + first-line title (the same 200-char
/// derivation the FTS index uses) + bounded content.
fn note_prompt(note: &Note) -> String {
    let title: String = note
        .content
        .lines()
        .next()
        .unwrap_or(&note.content)
        .chars()
        .take(200)
        .collect();
    let content = truncate_chars(&note.content, MAX_NOTE_CHARS);
    format!(
        "Note id: {}\nTitle: {}\n\nExtract the notable notions from the note below. Reply with ONLY the JSON array.\n\n---\n{content}\n---",
        note.id, title
    )
}

/// Rough token estimate: ~4 chars per token with a 64-token floor, plus the
/// reserved reply budget.
fn estimate_tokens(content: &str) -> u32 {
    (content.chars().count() as u32 / 4).max(64) + REPLY_ESTIMATE_TOKENS
}

/// Char-boundary-safe truncation (never splits a UTF-8 code point).
fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_note(content: &str) -> Note {
        Note {
            id: "note-1".to_string(),
            content: content.to_string(),
            ..Note::default()
        }
    }

    #[tokio::test]
    async fn no_model_yields_zero_enrichment_and_zero_tokens() {
        let mut stage = LlmDistillStage::new(LoopBudget::default(), None);
        let notes = vec![make_note("rust and sqlite notes")];
        let (notions, tokens, cost) = stage.distill_with_fallback(&notes).await.unwrap();
        assert!(notions.is_empty());
        assert_eq!(tokens, 0);
        assert_eq!(cost, 0.0, "no model configured — no cost charged");
    }

    #[tokio::test]
    async fn budget_exhausted_stops_before_any_call() {
        // 1-token ceiling: the first reservation is refused → no call, no tokens.
        let mut stage = LlmDistillStage::new(LoopBudget::with_limits(5, 1), None);
        let notes = vec![make_note("rust and sqlite notes")];
        let (notions, tokens, cost) = stage.distill_with_fallback(&notes).await.unwrap();
        assert!(notions.is_empty());
        assert_eq!(tokens, 0);
        assert_eq!(cost, 0.0, "budget refused the call — nothing billed");
    }

    /// End-to-end fail-open through the real router plumbing: the mock
    /// provider answers with a non-JSON "[mock] ..." line, the parser must
    /// drop it and the reservation must still be accounted.
    #[tokio::test]
    async fn mock_provider_garbage_reply_fails_open() {
        // Edition 2024: set_var is unsafe (process-global mutation).
        unsafe { std::env::set_var("ZEN_LOOP_DISTILL_ALLOW_CLOUD", "1") };
        zen_core::config::invalidate_config_cache();
        let mut stage = LlmDistillStage::new(LoopBudget::default(), Some("mock".to_string()));
        let notes = vec![make_note("rust and sqlite notes")];
        let (notions, tokens, cost) = stage.distill_with_fallback(&notes).await.unwrap();
        assert!(
            notions.is_empty(),
            "garbage reply must parse to zero notions"
        );
        assert!(
            cost >= 0.0,
            "fail-open reservation must still charge (0 on unpriced mock)"
        );
        assert!(
            tokens > 0,
            "the reservation must be accounted even on fail-open"
        );
    }

    #[tokio::test]
    async fn merge_section_unknown_provider_fails_open_to_none() {
        unsafe { std::env::set_var("ZEN_LOOP_DISTILL_ALLOW_CLOUD", "1") };
        zen_core::config::invalidate_config_cache();
        let section = merge_section(
            "definitely-not-a-provider",
            "target",
            "source",
            "some unique line",
        )
        .await;
        assert!(
            section.is_none(),
            "unconfigured provider must fail open to raw append"
        );
    }

    #[test]
    fn parse_accepts_valid_entries_and_sets_fields() {
        let raw = r#" preamble [ {"name": "SQLite", "kind": "Technology", "description": "embedded db", "aliases": ["sqlite3"], "topics": ["db"]} ] trailing"#;
        let notions = parse_llm_notions(raw, "n1", 8);
        assert_eq!(notions.len(), 1);
        assert_eq!(notions[0].name, "SQLite");
        assert_eq!(notions[0].description, "embedded db");
        assert_eq!(notions[0].aliases, vec!["sqlite3"]);
        assert_eq!(notions[0].topics, vec!["db"]);
        assert_eq!(notions[0].source_note_id, "n1");
    }

    #[test]
    fn parse_drops_invalid_kind_and_empty_name_and_caps() {
        let raw = r#"[
            {"name": "Ghost", "kind": "alien-tech"},
            {"name": "", "kind": "concept"},
            {"kind": "concept"},
            {"name": "A", "kind": "concept"},
            {"name": "B", "kind": "concept"},
            {"name": "C", "kind": "concept"}
        ]"#;
        let notions = parse_llm_notions(raw, "n1", 2);
        assert_eq!(
            notions.len(),
            2,
            "invalid entries dropped, output capped at max"
        );
        assert_eq!(notions[0].name, "A");
        assert_eq!(notions[1].name, "B");
    }

    #[test]
    fn parse_fail_open_on_no_array_and_on_non_json() {
        assert!(parse_llm_notions("no brackets here", "n1", 8).is_empty());
        assert!(parse_llm_notions("[not json]", "n1", 8).is_empty());
        assert!(parse_llm_notions("[]", "n1", 8).is_empty());
    }

    #[test]
    fn estimate_has_floor_and_reply_budget() {
        assert_eq!(estimate_tokens(""), 64 + REPLY_ESTIMATE_TOKENS);
        assert_eq!(estimate_tokens("x"), 64 + REPLY_ESTIMATE_TOKENS);
        assert_eq!(
            estimate_tokens(&"a".repeat(400)),
            100 + REPLY_ESTIMATE_TOKENS
        );
    }

    #[test]
    fn billable_uses_echo_and_never_rebills_cache_reads() {
        let mut usage = CacheUsage::default();
        assert_eq!(
            billable_tokens(&usage, 777),
            777,
            "no echo → estimate fallback"
        );
        usage.input_tokens = Some(1000);
        usage.output_tokens = Some(50);
        usage.cache_creation_input_tokens = Some(200);
        usage.cache_read_input_tokens = Some(800);
        // 50 output + 200 creation + (1000 input − 800 cache-read) = 450.
        assert_eq!(billable_tokens(&usage, 777), 450);
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_chars("hi", 10), "hi");
        assert_eq!(truncate_chars("你好世界", 2), "你好");
    }
}
