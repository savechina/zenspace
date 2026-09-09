//! Intent classification (001-agentic-foundation A.3, convergence T372).
//!
//! Pipeline order — **LLM first, keyword is the degraded fast path**: a
//! cheap LLM classification turn runs at the top of every orchestrator
//! turn; only when the LLM is unavailable (routing error, stream error,
//! timeout, unparsable reply) does the keyword table take over. A
//! low-confidence LLM verdict (<0.7) and a keyword-miss both fall back to
//! `Conversation`, mirroring 001's "unknown → Conversation fallback".
//!
//! # Functionality
//! Produces an [`Intent`] (category + ACL + confidence + provenance) used
//! for agent routing and the `loop.turn.review` audit line.
//! # User impact
//! Ambiguous requests now reach a model-driven classification instead of a
//! substring match; every degrade is observable in logs + audit.
//! # Default behavior
//! LLM-first, 20s wall-clock cap, `>=0.7` confidence gate.
//! # Interaction
//! `keyword_route` is reused verbatim by `AgentOrchestrator::classify_intent`
//! for the synchronous `route()` facade; the async [`classify`] is the
//! execute/execute_stream path.

use std::time::Duration;

use tracing::warn;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouter as _, TaskRequirements};

/// 006 D6: intent signal table salvaged from the deleted ZenCoordinator's
/// RoutingRules — single source of truth for keyword routing. Order is
/// precedence: first matching row wins, falling back to "Sisyphus".
pub(crate) const INTENT_SIGNALS: &[(&str, &str, &[&str])] = &[
    (
        "Hephaestus",
        "coder",
        &[
            "implement",
            "code",
            "function",
            "class",
            "refactor",
            "debug",
        ],
    ),
    (
        "Explore",
        "research",
        &[
            "research",
            "explore",
            "discover",
            "find information",
            "investigate",
        ],
    ),
    (
        "Oracle",
        "deep-analysis",
        &["analyze", "analysis", "deep", "architecture", "design"],
    ),
    (
        "Librarian",
        "knowledge-org",
        &["organize", "knowledge", "wiki", "notes", "catalog", "dedup"],
    ),
    (
        "Hermes",
        "consolidate-pipeline",
        &["consolidate", "pipeline", "merge", "compile wiki"],
    ),
    (
        "Momus",
        "review-quality",
        &["review", "audit", "check quality", "security"],
    ),
    (
        "Prometheus",
        "planning",
        &["plan", "strategy", "roadmap", "spec", "milestone"],
    ),
    (
        "Metis",
        "gap-assessment",
        &["gap", "tactical", "assumption", "feasibility"],
    ),
    (
        "Atlas",
        "batch-automation",
        &["batch", "automate", "routine", "schedule"],
    ),
    (
        "Junior",
        "format-convert",
        &["format", "convert", "download", "clean"],
    ),
    (
        "Zeus",
        "value-alignment",
        &["value", "align", "priority", "should we"],
    ),
    (
        "Argus",
        "visual",
        &["image", "chart", "visual", "diagram", "screenshot"],
    ),
];

/// Wall-clock cap for the LLM classification turn. A hung provider must not
/// stall every turn's first token; degrade to the keyword path instead.
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);

/// 001 A.3: LLM verdicts below this confidence fall back to Conversation
/// (gate is inclusive — exactly 0.7 is accepted).
const CONFIDENCE_GATE: f32 = 0.7;

/// 001 A.3 intent categories. `Query`/`Action`/`System` map onto the ACL
/// ladder; `Conversation` is the unprivileged fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentCategory {
    Query,
    Action,
    System,
    Conversation,
}

impl IntentCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Query => "Query",
            Self::Action => "Action",
            Self::System => "System",
            Self::Conversation => "Conversation",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "query" | "read" | "search" => Some(Self::Query),
            "action" | "write" | "execute" => Some(Self::Action),
            "system" | "admin" | "config" => Some(Self::System),
            "conversation" | "chat" | "help" => Some(Self::Conversation),
            _ => None,
        }
    }

    /// Category-derived default handler when the LLM path routes the turn
    /// (001 A.3 sub-agent column: Query→search-agent, Action→note-agent,
    /// System/Conversation→orchestrator; zen's registry equivalents).
    fn default_agent(&self) -> &'static str {
        match self {
            Self::Query => "Explore",
            Self::Action => "Hephaestus",
            Self::System | Self::Conversation => "Sisyphus",
        }
    }
}

/// 001 A.3 ACL ladder (category → permission class). Recorded on the
/// `loop.turn.review` audit line; enforcement stays with the existing
/// sensitivity routing + sandbox hooks (no new gate in this change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acl {
    ReadOnly,
    Write,
    Admin,
    None,
}

impl Acl {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Write => "write",
            Self::Admin => "admin",
            Self::None => "none",
        }
    }

    fn from_category(category: IntentCategory) -> Self {
        match category {
            IntentCategory::Query => Self::ReadOnly,
            IntentCategory::Action => Self::Write,
            IntentCategory::System => Self::Admin,
            IntentCategory::Conversation => Self::None,
        }
    }
}

/// Where the verdict came from — audit provenance for the degrade ladder
/// (Llm → Keyword → Fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSource {
    Llm,
    Keyword,
    Fallback,
}

/// One classified user turn (001 A.3 `Intent {category, acl_check,
/// confidence}` plus zen's agent/signal routing fields).
#[derive(Debug, Clone)]
pub struct Intent {
    pub category: IntentCategory,
    pub agent: String,
    pub signal: String,
    pub acl: Acl,
    pub confidence: f32,
    pub source: IntentSource,
}

impl Intent {
    /// Conversation fallback (001 A.3: unknown or low-confidence intent).
    fn fallback(signal: &str, confidence: f32) -> Self {
        Self {
            category: IntentCategory::Conversation,
            agent: "Sisyphus".to_string(),
            signal: signal.to_string(),
            acl: Acl::from_category(IntentCategory::Conversation),
            confidence,
            source: IntentSource::Fallback,
        }
    }

    fn from_category(category: IntentCategory, confidence: f32, source: IntentSource) -> Self {
        Self {
            category,
            agent: category.default_agent().to_string(),
            signal: category.as_str().to_lowercase(),
            acl: Acl::from_category(category),
            confidence,
            source,
        }
    }
}

/// Keyword fast path over [`INTENT_SIGNALS`] — degraded-mode routing when
/// the LLM classifier is unavailable, and the synchronous `route()`
/// facade. Returns `None` when no keyword matches.
pub(crate) fn keyword_route(query: &str) -> Option<Intent> {
    let lower = query.to_lowercase();
    for (agent, signal, keywords) in INTENT_SIGNALS {
        if keywords.iter().any(|kw| lower.contains(kw)) {
            let (category, confidence) = keyword_signal_category(signal);
            return Some(Intent {
                agent: (*agent).to_string(),
                signal: (*signal).to_string(),
                category,
                acl: Acl::from_category(category),
                confidence,
                source: IntentSource::Keyword,
            });
        }
    }
    None
}

/// Signal → category heuristic for the keyword path: signals that mutate
/// state map to `Action`, analytical ones to `Query`.
fn keyword_signal_category(signal: &str) -> (IntentCategory, f32) {
    let category = match signal {
        "coder" | "format-convert" | "batch-automation" | "consolidate-pipeline" => {
            IntentCategory::Action
        }
        _ => IntentCategory::Query,
    };
    (category, 0.8)
}

/// Classify one user turn: LLM first, keyword fast path on LLM failure,
/// Conversation fallback on low confidence or keyword miss.
pub async fn classify(router: &DefaultRouter, query: &str, sensitivity: Sensitivity) -> Intent {
    let verdict = llm_classify(router, query, sensitivity).await;
    resolve_intent(verdict, query)
}

/// Pure degrade ladder (001 A.3), extracted so every rung is unit-testable
/// without a live router (/review L1): an accepted LLM verdict routes as
/// [`IntentSource::Llm`]; a parsed-but-low-confidence verdict falls back
/// to Conversation; any routing/stream/timeout/unparsable error degrades
/// to the keyword fast path, then to the Conversation fallback on a
/// keyword miss.
fn resolve_intent(verdict: Result<Option<(IntentCategory, f32)>, String>, query: &str) -> Intent {
    match verdict {
        Ok(Some((category, confidence))) => {
            Intent::from_category(category, confidence, IntentSource::Llm)
        }
        Ok(None) => {
            warn!("intent LLM verdict below {CONFIDENCE_GATE}; falling back to Conversation");
            Intent::fallback("low-confidence", 0.5)
        }
        Err(err) => {
            warn!(
                error = %err,
                "LLM intent classification unavailable; degrading to keyword fast path"
            );
            keyword_route(query).unwrap_or_else(|| Intent::fallback("default", 0.5))
        }
    }
}

/// Inclusive confidence gate (001 A.3): exactly [`CONFIDENCE_GATE`] passes.
fn passes_confidence_gate(confidence: f32) -> bool {
    confidence >= CONFIDENCE_GATE
}

/// One non-streaming-equivalent LLM round over the streaming path (async
/// safe — no nested runtime). Returns `Ok(None)` when the reply parsed but
/// the confidence gate rejected it; `Err` degrades to the keyword path.
async fn llm_classify(
    router: &DefaultRouter,
    query: &str,
    sensitivity: Sensitivity,
) -> Result<Option<(IntentCategory, f32)>, String> {
    let requirements = TaskRequirements {
        max_tokens: Some(128),
        sensitivity,
        preferred_model: None,
        budget_limit: None,
    };
    // `route()` is a sync trait method whose sensitivity enforcement may run
    // a blocking provider health check (nested `Runtime::new` inside
    // OllamaProvider) — panic-safe only off the async runtime.
    let route_router = router.clone();
    let provider = tokio::task::spawn_blocking(move || route_router.route(&requirements))
        .await
        .map_err(|e| format!("classify route join: {e}"))?
        .map_err(|e| format!("route: {e}"))?;
    let prompt = format!(
        "Classify the user's request into exactly one category.\n\
         Categories: \"Query\" (search or read knowledge), \"Action\" (create, modify, or execute something), \"System\" (manage configuration or services), \"Conversation\" (chat, help, or clarification).\n\
         User request: {query}\n\
         Respond with ONLY a JSON object: {{\"category\": \"<Category>\", \"confidence\": <0.0-1.0>}}"
    );
    let reply = tokio::time::timeout(CLASSIFY_TIMEOUT, async {
        let mut stream = router
            .call_stream(provider, &prompt)
            .map_err(|e| format!("stream: {e}"))?;
        let mut reply = String::new();
        while let Some(token) = stream.token_rx.recv().await {
            reply.push_str(&token);
        }
        if let Ok(Err(e)) = stream.done_rx.await {
            return Err(e);
        }
        Ok(reply)
    })
    .await
    .map_err(|_| "classification timed out".to_string())??;

    let (category, confidence) = parse_classification(&reply)
        .ok_or_else(|| format!("unparsable classification reply: {reply}"))?;
    if !passes_confidence_gate(confidence) {
        return Ok(None);
    }
    Ok(Some((category, confidence)))
}

/// Parse `{"category": "...", "confidence": 0.xx}` out of a possibly noisy
/// reply (fences, prose). Pure — unit-tested below.
fn parse_classification(reply: &str) -> Option<(IntentCategory, f32)> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let category = IntentCategory::parse(value.get("category")?.as_str()?)?;
    let confidence = value.get("confidence")?.as_f64()? as f32;
    Some((category, confidence.clamp(0.0, 1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classification_accepts_clean_fenced_and_noisy_replies() {
        let clean = parse_classification(r#"{"category": "Query", "confidence": 0.92}"#);
        assert_eq!(clean, Some((IntentCategory::Query, 0.92)));

        let fenced =
            parse_classification("```json\n{\"category\": \"action\", \"confidence\": 0.7}\n```");
        assert_eq!(fenced, Some((IntentCategory::Action, 0.7)));

        let noisy = parse_classification(
            "Sure! Here is the classification: {\"category\": \"System\", \"confidence\": 1.5} hope that helps",
        );
        assert_eq!(noisy, Some((IntentCategory::System, 1.0)));
    }

    #[test]
    fn parse_classification_rejects_garbage_unknown_category_and_missing_fields() {
        assert_eq!(parse_classification("no json here"), None);
        assert_eq!(
            parse_classification("{\"category\": \"Vibes\", \"confidence\": 0.9}"),
            None
        );
        assert_eq!(parse_classification("{\"confidence\": 0.9}"), None);
        assert_eq!(parse_classification("{\"category\": \"Query\"}"), None);
    }

    #[test]
    fn keyword_route_maps_signals_to_categories_and_agents() {
        let coder = keyword_route("please refactor this function").expect("coder hit");
        assert_eq!(coder.agent, "Hephaestus");
        assert_eq!(coder.category, IntentCategory::Action);
        assert_eq!(coder.acl, Acl::Write);
        assert_eq!(coder.source, IntentSource::Keyword);

        let research = keyword_route("research the tokio runtime").expect("research hit");
        assert_eq!(research.agent, "Explore");
        assert_eq!(research.category, IntentCategory::Query);
        assert_eq!(research.acl, Acl::ReadOnly);

        assert!(
            keyword_route("what can you do?").is_none(),
            "no keyword hit"
        );
    }

    #[test]
    fn fallback_intent_is_conversation_with_sisyphus() {
        let intent = Intent::fallback("default", 0.5);
        assert_eq!(intent.category, IntentCategory::Conversation);
        assert_eq!(intent.agent, "Sisyphus");
        assert_eq!(intent.acl, Acl::None);
        assert_eq!(intent.source, IntentSource::Fallback);
    }

    #[tokio::test]
    async fn classify_degrades_to_keyword_path_when_llm_reply_is_not_json() {
        let router = zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: Some("mock".to_string()),
            ..Default::default()
        });
        // Mock replies with "[mock] ..." (no JSON) → keyword degradation.
        let intent = classify(
            &router,
            "please refactor this function",
            Sensitivity::Public,
        )
        .await;
        assert_eq!(intent.source, IntentSource::Keyword);
        assert_eq!(intent.agent, "Hephaestus");

        let no_hit = classify(&router, "tell me a joke about rust", Sensitivity::Public).await;
        assert_eq!(no_hit.source, IntentSource::Fallback);
        assert_eq!(no_hit.agent, "Sisyphus");
        assert_eq!(no_hit.category, IntentCategory::Conversation);
    }
    #[test]
    fn resolve_intent_accepts_confident_llm_verdict() {
        let intent = resolve_intent(Ok(Some((IntentCategory::Action, 0.9))), "anything");
        assert_eq!(intent.source, IntentSource::Llm);
        assert_eq!(intent.category, IntentCategory::Action);
        assert_eq!(intent.acl, Acl::Write);
        assert_eq!(intent.agent, "Hephaestus");
    }

    #[test]
    fn resolve_intent_falls_back_to_conversation_on_low_confidence() {
        let intent = resolve_intent(Ok(None), "please refactor this function");
        assert_eq!(intent.source, IntentSource::Fallback);
        assert_eq!(intent.category, IntentCategory::Conversation);
        assert_eq!(intent.agent, "Sisyphus");
        assert_eq!(intent.acl, Acl::None);
        assert_eq!(intent.confidence, 0.5);
    }

    #[test]
    fn resolve_intent_degrades_errors_to_keyword_then_conversation() {
        // Routing / stream / timeout failures all arrive as Err.
        let timed_out = resolve_intent(
            Err("classification timed out".to_string()),
            "plan the roadmap",
        );
        assert_eq!(timed_out.source, IntentSource::Keyword);
        assert_eq!(timed_out.agent, "Prometheus");

        let miss = resolve_intent(Err("route: no provider".to_string()), "tell me a joke");
        assert_eq!(miss.source, IntentSource::Fallback);
        assert_eq!(miss.category, IntentCategory::Conversation);
    }

    #[test]
    fn confidence_gate_is_inclusive_at_threshold() {
        assert!(passes_confidence_gate(0.7));
        assert!(passes_confidence_gate(1.0));
        assert!(!passes_confidence_gate(0.69));
    }

    #[test]
    fn category_parse_accepts_aliases_and_is_case_insensitive() {
        assert_eq!(IntentCategory::parse("read"), Some(IntentCategory::Query));
        assert_eq!(IntentCategory::parse("SEARCH"), Some(IntentCategory::Query));
        assert_eq!(IntentCategory::parse("write"), Some(IntentCategory::Action));
        assert_eq!(
            IntentCategory::parse("Execute"),
            Some(IntentCategory::Action)
        );
        assert_eq!(IntentCategory::parse("admin"), Some(IntentCategory::System));
        assert_eq!(
            IntentCategory::parse("config"),
            Some(IntentCategory::System)
        );
        assert_eq!(
            IntentCategory::parse("chat"),
            Some(IntentCategory::Conversation)
        );
        assert_eq!(
            IntentCategory::parse("Help"),
            Some(IntentCategory::Conversation)
        );
        assert_eq!(
            IntentCategory::parse("  Query  "),
            Some(IntentCategory::Query)
        );
        assert_eq!(IntentCategory::parse("vibes"), None);
    }
}
