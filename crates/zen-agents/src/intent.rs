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
//!
//! # Routing scope (eng-review D4, accepted behavior)
//! The LLM path maps categories to category-default agents
//! (`default_agent`: Query→Explore, Action→Hephaestus,
//! System/Conversation→Sisyphus); only the keyword path routes across the
//! full INTENT_SIGNALS catalog. Consequence: `delegate.task` and
//! `plan.execute` live on Sisyphus alone, so they are reachable on the LLM
//! path only via System/Conversation/low-confidence fallback — a
//! confidently-classified Query/Action turn runs as a leaf agent without
//! delegation grants. Routing target for the same query can therefore
//! differ between classifier-available and degraded runs. Changing this is
//! a product decision (specialist ownership vs. always-orchestrate), not a
//! bug fix.
//!
//! # LLM-path worthiness (T112)
//! Every classify attempt records an [`LlmOutcome`] (`ok`, `low_confidence`,
//! `timeout`, `unavailable`, `error`, `skipped`) plus `elapsed_ms` into the
//! `intent_llm_outcome` / `intent_llm_ms` fields on the `loop.turn.review`
//! audit line. A config-level fail-fast gate (no network probe) skips the LLM
//! attempt entirely when no provider is configured, emitting `skipped` with
//! zero timeout burn.
//!
//! **Decision rule** (V8 §6, after PD-01 lands): the outcome distribution
//! snapshot decides keep-LLM-first vs ambiguity-only-trigger vs retire —
//! see V8.md §6 "snapshot" section for the evaluation criteria.

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
    ///
    /// Since T169 this is only the *fallback* — a resolved signal wins over it
    /// (see [`agent_for_signal`]), which is what makes all 13 registered
    /// agents reachable from every ladder rung.
    pub(crate) fn default_agent(&self) -> &'static str {
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
/// (L1 → Llm → Keyword → Fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSource {
    /// L1 — calibrated decision layer (embedding router over the seed intent
    /// table, or the constrained local classifier). Only reachable when a
    /// calibrated threshold is supplied (T169); absent one, the ladder never
    /// produces this source and the pre-T169 behaviour is preserved.
    L1,
    Llm,
    Keyword,
    Fallback,
}

/// Outcome of the LLM classification attempt (T112). Recorded on the
/// `loop.turn.review` audit line as `intent_llm_outcome` to make the
/// LLM-first path's worthiness measurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmOutcome {
    /// LLM replied with confidence >= 0.7 — routed as IntentSource::Llm.
    Ok,
    /// LLM replied but confidence < 0.7 — fell back to Conversation.
    LowConfidence,
    /// LLM classify exceeded the 20s wall-clock cap.
    Timeout,
    /// Provider/model or API key could not be resolved (config-level).
    Unavailable,
    /// Other LLM error (stream failure, unparsable reply, join error).
    Error,
    /// LLM attempt skipped by the fail-fast availability gate (no provider
    /// configured) — zero timeout burn.
    Skipped,
    /// LLM attempt not made because the L1 rung already resolved the decision
    /// above the calibrated threshold (T169) — zero LLM cost. This is the
    /// outcome the ladder's fast path is meant to produce; its share of
    /// `loop.turn.review` lines is the V13-A.3 "<10% LLM-intent rate" baseline.
    L1Resolved,
}

impl LlmOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::LowConfidence => "low_confidence",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
            Self::Skipped => "skipped",
            Self::L1Resolved => "l1_resolved",
        }
    }
}

/// Telemetry for one LLM classification attempt (T112). Attached to the
/// `loop.turn.review` audit line as `intent_llm_outcome` + `intent_llm_ms`.
#[derive(Debug, Clone)]
pub struct LlmTelemetry {
    pub outcome: LlmOutcome,
    pub elapsed_ms: u64,
}

impl LlmTelemetry {
    fn skipped() -> Self {
        Self {
            outcome: LlmOutcome::Skipped,
            elapsed_ms: 0,
        }
    }
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

    /// Build an intent from a ladder rung's verdict.
    ///
    /// A resolved signal decides the agent ([`agent_for_signal`]); the
    /// category default is only the fallback, which is what makes all 13
    /// registered agents reachable from every rung (T169). An unresolvable
    /// signal is still recorded verbatim — it is audit provenance — but does
    /// not change the agent.
    pub(crate) fn from_ladder(
        category: IntentCategory,
        signal: Option<&str>,
        confidence: f32,
        source: IntentSource,
    ) -> Self {
        let agent = signal
            .and_then(agent_for_signal)
            .unwrap_or_else(|| category.default_agent());
        let signal_name = match signal {
            Some(value) => value.to_string(),
            None => category.as_str().to_lowercase(),
        };
        Self {
            category,
            agent: agent.to_string(),
            signal: signal_name,
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
pub(crate) fn keyword_signal_category(signal: &str) -> (IntentCategory, f32) {
    let category = match signal {
        "coder" | "format-convert" | "batch-automation" | "consolidate-pipeline" => {
            IntentCategory::Action
        }
        _ => IntentCategory::Query,
    };
    (category, 0.8)
}

/// Resolve a concrete agent from an `INTENT_SIGNALS` signal name.
///
/// T169: this is what dissolves the expressiveness inversion. Before it, the
/// LLM path reached only the 3 category-default agents (`default_agent`)
/// while the keyword path reached all 12 signals, so a confidently-classified
/// Query/Action turn could never land on a specialist. Every rung's verdict
/// now carries an optional signal and a resolved signal wins over the
/// category default, making all 13 registered agents reachable from every
/// rung. Unknown/absent signals return `None` and the caller falls back.
pub(crate) fn agent_for_signal(signal: &str) -> Option<&'static str> {
    INTENT_SIGNALS
        .iter()
        .find(|(_, sig, _)| *sig == signal)
        .map(|(agent, _, _)| *agent)
}

/// The selectable signal names, comma-separated — used to state the LLM
/// rung's signal vocabulary in its prompt so the model can pick a specialist
/// rather than only a category.
pub(crate) fn signal_vocabulary() -> String {
    INTENT_SIGNALS
        .iter()
        .map(|(_, signal, _)| *signal)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Classify one user turn: LLM first, keyword fast path on LLM failure,
/// Conversation fallback on low confidence or keyword miss. Returns the
/// classified intent plus [`LlmTelemetry`] for the audit line.
pub async fn classify(
    router: &DefaultRouter,
    query: &str,
    sensitivity: Sensitivity,
) -> (Intent, LlmTelemetry) {
    // T112 (c): fail-fast availability gate — config-level only, no
    // network probe. If no provider is configured, skip the LLM attempt
    // entirely and go straight to the keyword path.
    if !router.has_configured_provider() {
        let intent = keyword_route(query).unwrap_or_else(|| Intent::fallback("default", 0.5));
        return (intent, LlmTelemetry::skipped());
    }
    let start = std::time::Instant::now();
    let verdict = llm_classify(router, query, sensitivity).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (intent, outcome) = resolve_intent(verdict, query);
    (
        intent,
        LlmTelemetry {
            outcome,
            elapsed_ms,
        },
    )
}

/// Pure degrade ladder (001 A.3), extracted so every rung is unit-testable
/// without a live router (/review L1): an accepted LLM verdict routes as
/// [`IntentSource::Llm`]; a parsed-but-low-confidence verdict falls back
/// to Conversation; any routing/stream/timeout/unparsable error degrades
/// to the keyword fast path, then to the Conversation fallback on a
/// keyword miss.
fn resolve_intent(
    verdict: Result<Option<(IntentCategory, f32, Option<String>)>, String>,
    query: &str,
) -> (Intent, LlmOutcome) {
    match verdict {
        Ok(Some((category, confidence, signal))) => (
            Intent::from_ladder(category, signal.as_deref(), confidence, IntentSource::Llm),
            LlmOutcome::Ok,
        ),
        Ok(None) => {
            warn!("intent LLM verdict below {CONFIDENCE_GATE}; falling back to Conversation");
            (
                Intent::fallback("low-confidence", 0.5),
                LlmOutcome::LowConfidence,
            )
        }
        Err(err) => {
            let outcome = classify_error_outcome(&err);
            warn!(
                error = %err,
                outcome = outcome.as_str(),
                "LLM intent classification unavailable; degrading to keyword fast path"
            );
            let intent = keyword_route(query).unwrap_or_else(|| Intent::fallback("default", 0.5));
            (intent, outcome)
        }
    }
}

/// Map an LLM classify error string to the appropriate [`LlmOutcome`].
fn classify_error_outcome(err: &str) -> LlmOutcome {
    if err.contains("timed out") {
        LlmOutcome::Timeout
    } else if err.contains("route:") || err.contains("no provider") {
        LlmOutcome::Unavailable
    } else {
        LlmOutcome::Error
    }
}

/// Inclusive confidence gate (001 A.3): exactly [`CONFIDENCE_GATE`] passes.
fn passes_confidence_gate(confidence: f32) -> bool {
    confidence >= CONFIDENCE_GATE
}

/// One prompt → one model reply over the streaming path (async safe — no
/// nested runtime). Callers own prompt construction and output parsing, so
/// every typed contract in the crate (intent classification, the T171 binary
/// classifiers) shares one plumbing implementation instead of forking it.
pub(crate) async fn complete_prompt(
    router: &DefaultRouter,
    prompt: &str,
    sensitivity: Sensitivity,
    max_tokens: u32,
) -> Result<String, String> {
    let requirements = TaskRequirements {
        max_tokens: Some(max_tokens),
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
    let prompt = prompt.to_string();
    tokio::time::timeout(CLASSIFY_TIMEOUT, async {
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
    .map_err(|_| "classification timed out".to_string())?
}

/// The category+signal classification prompt. Pure, so the contract it states
/// is testable without a provider.
pub(crate) fn classification_prompt(query: &str) -> String {
    format!(
        "Classify the user's request into exactly one category and, when one clearly applies, one routing signal.\n\
         Categories: \"Query\" (search or read knowledge), \"Action\" (create, modify, or execute something), \"System\" (manage configuration or services), \"Conversation\" (chat, help, or clarification).\n\
         Signals: {}\n\
         User request: {query}\n\
         Respond with ONLY a JSON object: {{\"category\": \"<Category>\", \"confidence\": <0.0-1.0>, \"signal\": \"<signal-or-omit>\"}}",
        signal_vocabulary()
    )
}

/// One typed classification round: one model reply parsed and validated into
/// `(category, confidence, signal)`.
///
/// No confidence gate is applied here. [`llm_classify`] layers the L2 verbal
/// gate on top, while the L1 classifier rung consumes the raw score and lets
/// the ladder's calibrated threshold decide — one contract implementation,
/// two gates over it (no forked call path).
///
/// The typed contract is enforced by parse-and-validate, **not** by
/// constrained decoding: the provider layer exposes no grammar/`format`
/// passthrough, so a malformed reply is rejected here and the caller retries.
pub(crate) async fn classify_typed(
    router: &DefaultRouter,
    query: &str,
    sensitivity: Sensitivity,
) -> Result<Option<(IntentCategory, f32, Option<String>)>, String> {
    let reply = complete_prompt(router, &classification_prompt(query), sensitivity, 128).await?;
    parse_classification(&reply)
        .map(Some)
        .ok_or_else(|| format!("unparsable classification reply: {reply}"))
}

/// L2's call: [`classify_typed`] plus the 001 A.3 verbal-confidence gate
/// (exactly [`CONFIDENCE_GATE`] passes). `Ok(None)` means "parsed but below
/// the gate"; `Err` degrades to the keyword fast path.
async fn llm_classify(
    router: &DefaultRouter,
    query: &str,
    sensitivity: Sensitivity,
) -> Result<Option<(IntentCategory, f32, Option<String>)>, String> {
    match classify_typed(router, query, sensitivity).await? {
        Some((_, confidence, _)) if !passes_confidence_gate(confidence) => Ok(None),
        other => Ok(other),
    }
}

/// Parse `{"category": "...", "confidence": 0.xx, "signal": "..."}` out of a
/// possibly noisy reply (fences, prose). Pure — unit-tested below.
///
/// `signal` is optional (a model that omits it degrades to the category
/// default, i.e. pre-T169 behaviour) and is **only accepted when it names a
/// real `INTENT_SIGNALS` signal** — a hallucinated signal can never route to
/// an agent, it is simply dropped.
fn parse_classification(reply: &str) -> Option<(IntentCategory, f32, Option<String>)> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let category = IntentCategory::parse(value.get("category")?.as_str()?)?;
    let confidence = value.get("confidence")?.as_f64()? as f32;
    let signal = value
        .get("signal")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|signal| agent_for_signal(signal).is_some())
        .map(str::to_string);
    Some((category, confidence.clamp(0.0, 1.0), signal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_classification_accepts_clean_fenced_and_noisy_replies() {
        let clean = parse_classification(r#"{"category": "Query", "confidence": 0.92}"#);
        assert_eq!(clean, Some((IntentCategory::Query, 0.92, None)));

        let fenced =
            parse_classification("```json\n{\"category\": \"action\", \"confidence\": 0.7}\n```");
        assert_eq!(fenced, Some((IntentCategory::Action, 0.7, None)));

        let noisy = parse_classification(
            "Sure! Here is the classification: {\"category\": \"System\", \"confidence\": 1.5} hope that helps",
        );
        assert_eq!(noisy, Some((IntentCategory::System, 1.0, None)));
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
    fn parse_classification_accepts_known_signal_and_drops_hallucinated_ones() {
        let known = parse_classification(
            r#"{"category": "Query", "confidence": 0.9, "signal": "deep-analysis"}"#,
        );
        assert_eq!(
            known,
            Some((
                IntentCategory::Query,
                0.9,
                Some("deep-analysis".to_string())
            ))
        );

        let hallucinated = parse_classification(
            r#"{"category": "Query", "confidence": 0.9, "signal": "chief-vibes-officer"}"#,
        );
        assert_eq!(
            hallucinated,
            Some((IntentCategory::Query, 0.9, None)),
            "an unknown signal can never route to an agent"
        );
    }

    #[test]
    fn every_signal_resolves_to_a_distinct_registered_agent() {
        let mut agents = std::collections::BTreeSet::new();
        for (_, signal, _) in INTENT_SIGNALS {
            let agent = agent_for_signal(signal).expect("every table signal resolves");
            agents.insert(agent);
        }
        assert_eq!(
            agents.len(),
            INTENT_SIGNALS.len(),
            "signal→agent mapping must be injective"
        );
        assert_eq!(agent_for_signal("nope"), None);
    }

    #[test]
    fn llm_signal_wins_over_category_default_for_agent_selection() {
        let with_signal = Intent::from_ladder(
            IntentCategory::Query,
            Some("review-quality"),
            0.9,
            IntentSource::Llm,
        );
        assert_eq!(
            with_signal.agent, "Momus",
            "a resolved signal beats the Query default (Explore)"
        );
        assert_eq!(with_signal.signal, "review-quality");
        assert_eq!(with_signal.category, IntentCategory::Query);

        let without_signal =
            Intent::from_ladder(IntentCategory::Query, None, 0.9, IntentSource::Llm);
        assert_eq!(without_signal.agent, "Explore");
        assert_eq!(without_signal.signal, "query");

        let unresolvable = Intent::from_ladder(
            IntentCategory::Action,
            Some("not-a-signal"),
            0.9,
            IntentSource::Llm,
        );
        assert_eq!(unresolvable.agent, "Hephaestus");
        assert_eq!(
            unresolvable.signal, "not-a-signal",
            "recorded verbatim for audit, but does not route"
        );
    }

    #[test]
    fn all_thirteen_agents_are_reachable_from_signals() {
        // The registry's 13 agents: Sisyphus arrives via the category default
        // (System/Conversation), the other 12 via INTENT_SIGNALS.
        let mut reachable: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (_, signal, _) in INTENT_SIGNALS {
            reachable.insert(agent_for_signal(signal).expect("resolves"));
        }
        for category in [
            IntentCategory::Query,
            IntentCategory::Action,
            IntentCategory::System,
            IntentCategory::Conversation,
        ] {
            reachable.insert(category.default_agent());
        }
        for expected in [
            "Sisyphus",
            "Hephaestus",
            "Explore",
            "Oracle",
            "Librarian",
            "Hermes",
            "Momus",
            "Prometheus",
            "Metis",
            "Atlas",
            "Junior",
            "Zeus",
            "Argus",
        ] {
            assert!(
                reachable.contains(expected),
                "{expected} must be reachable from some rung"
            );
        }
        assert_eq!(reachable.len(), 13, "exactly the 13 registered agents");
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
        let (intent, telemetry) = classify(
            &router,
            "please refactor this function",
            Sensitivity::Public,
        )
        .await;
        assert_eq!(intent.source, IntentSource::Keyword);
        assert_eq!(intent.agent, "Hephaestus");
        assert_ne!(telemetry.outcome, LlmOutcome::Skipped);

        let (no_hit, _) = classify(&router, "tell me a joke about rust", Sensitivity::Public).await;
        assert_eq!(no_hit.source, IntentSource::Fallback);
        assert_eq!(no_hit.agent, "Sisyphus");
        assert_eq!(no_hit.category, IntentCategory::Conversation);
    }
    #[test]
    fn resolve_intent_accepts_confident_llm_verdict() {
        let (intent, outcome) =
            resolve_intent(Ok(Some((IntentCategory::Action, 0.9, None))), "anything");
        assert_eq!(intent.source, IntentSource::Llm);
        assert_eq!(intent.category, IntentCategory::Action);
        assert_eq!(intent.acl, Acl::Write);
        assert_eq!(intent.agent, "Hephaestus");
        assert_eq!(outcome, LlmOutcome::Ok);
    }

    #[test]
    fn resolve_intent_falls_back_to_conversation_on_low_confidence() {
        let (intent, outcome) = resolve_intent(Ok(None), "please refactor this function");
        assert_eq!(intent.source, IntentSource::Fallback);
        assert_eq!(intent.category, IntentCategory::Conversation);
        assert_eq!(intent.agent, "Sisyphus");
        assert_eq!(intent.acl, Acl::None);
        assert_eq!(intent.confidence, 0.5);
        assert_eq!(outcome, LlmOutcome::LowConfidence);
    }

    #[test]
    fn resolve_intent_degrades_errors_to_keyword_then_conversation() {
        // Routing / stream / timeout failures all arrive as Err.
        let (timed_out, outcome) = resolve_intent(
            Err("classification timed out".to_string()),
            "plan the roadmap",
        );
        assert_eq!(timed_out.source, IntentSource::Keyword);
        assert_eq!(timed_out.agent, "Prometheus");
        assert_eq!(outcome, LlmOutcome::Timeout);

        let (miss, outcome) =
            resolve_intent(Err("route: no provider".to_string()), "tell me a joke");
        assert_eq!(miss.source, IntentSource::Fallback);
        assert_eq!(miss.category, IntentCategory::Conversation);
        assert_eq!(outcome, LlmOutcome::Unavailable);
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

    #[test]
    fn classify_error_outcome_maps_timeout_unavailable_error() {
        assert_eq!(
            classify_error_outcome("classification timed out"),
            LlmOutcome::Timeout
        );
        assert_eq!(
            classify_error_outcome("route: no provider"),
            LlmOutcome::Unavailable
        );
        assert_eq!(
            classify_error_outcome("classify route join: task failed"),
            LlmOutcome::Error
        );
        assert_eq!(
            classify_error_outcome("unparsable classification reply: garbage"),
            LlmOutcome::Error
        );
    }

    #[tokio::test]
    async fn classify_skips_llm_when_no_provider_configured() {
        let router = zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: None,
            ..Default::default()
        });
        assert!(!router.has_configured_provider());

        let (intent, telemetry) = classify(
            &router,
            "please refactor this function",
            Sensitivity::Public,
        )
        .await;
        assert_eq!(telemetry.outcome, LlmOutcome::Skipped);
        assert_eq!(telemetry.elapsed_ms, 0);
        // Keyword path is byte-identical to the pre-change behavior.
        assert_eq!(intent.source, IntentSource::Keyword);
        assert_eq!(intent.agent, "Hephaestus");
        assert_eq!(intent.category, IntentCategory::Action);

        let (no_hit, telemetry) =
            classify(&router, "tell me a joke about rust", Sensitivity::Public).await;
        assert_eq!(telemetry.outcome, LlmOutcome::Skipped);
        assert_eq!(no_hit.source, IntentSource::Fallback);
        assert_eq!(no_hit.category, IntentCategory::Conversation);
    }

    #[test]
    fn llm_outcome_as_str_roundtrips() {
        assert_eq!(LlmOutcome::Ok.as_str(), "ok");
        assert_eq!(LlmOutcome::LowConfidence.as_str(), "low_confidence");
        assert_eq!(LlmOutcome::Timeout.as_str(), "timeout");
        assert_eq!(LlmOutcome::Unavailable.as_str(), "unavailable");
        assert_eq!(LlmOutcome::Error.as_str(), "error");
        assert_eq!(LlmOutcome::Skipped.as_str(), "skipped");
        assert_eq!(LlmOutcome::L1Resolved.as_str(), "l1_resolved");
    }
}
