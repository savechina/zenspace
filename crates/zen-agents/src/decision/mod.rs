//! L1 calibrated decision layer substrate (T168, V8.md 增补 A).
//!
//! # Functionality
//! The interchangeable-rung abstraction for the three-tier decision ladder:
//! L0 deterministic Rust → L1 calibrated decision layer (embedding router,
//! later a GBNF-constrained local classifier) → L2 LLM. Each rung implements
//! [`DecisionFn`] and returns a [`DecisionOutcome`] carrying `{ rung, choice,
//! confidence }`. The L0 and L2 rungs are thin adapters over the existing
//! `intent::keyword_route` / `intent::classify` paths — not forks.
//!
//! # User impact
//! Two modes. **Shadow (observation)**: the L1 rung runs after the production
//! decision and records a `loop.decision` audit line; it changes nothing.
//! **Ladder (T169)**: when — and only when — a calibrated threshold τ is
//! supplied (config `[agentic.intent] l1_threshold`, or T173's
//! `logs/decision-audit/thresholds.json`), L1 runs *before* the LLM and a
//! verdict at or above τ serves the turn, skipping the LLM classification
//! call. Signal-based agent resolution applies on every path, so all 13
//! registered agents are reachable from every rung.
//!
//! # Default behavior
//! Shadow observation is off (`[agentic.intent] shadow_embedding = false`) and
//! **no threshold exists**, so [`classify_intent`] delegates to the pre-T169
//! `intent::classify` path byte-for-byte. No τ is ever invented here
//! (V13-A.3): absent calibration data, every L1 gate stays closed.
//!
//! # Interaction
//! The embedder is injectable ([`TextEmbedder`]) so tests use a deterministic
//! fake without ONNX; production uses [`VaultTextEmbedder`] over the existing
//! local embedding path (`zen_vault::compute_embeddings_for_text`).
//! [`LocalClassifierRung`] abstains unless a local model is reachable and
//! otherwise routes through sensitivity enforcement in local mode, so no rung
//! can reach a cloud provider (`Sensitivity::Private` locality is a hard rule).

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::warn;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::DefaultRouter;

use crate::intent::{Intent, IntentCategory, IntentSource, LlmOutcome, LlmTelemetry};

/// Which rung of the decision ladder produced an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionRung {
    /// L0 — deterministic Rust (keyword/INTENT_SIGNALS, guards, budgets).
    L0,
    /// L1 — calibrated decision layer: the embedding router over the seed
    /// intent table, or [`LocalClassifierRung`]. Gates the decision only when
    /// a calibrated threshold is supplied (T169); without one it is
    /// observation-only and never influences routing.
    L1,
    /// L2 — LLM (the existing `intent::classify` path).
    L2,
}

impl DecisionRung {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::L0 => "L0",
            Self::L1 => "L1",
            Self::L2 => "L2",
        }
    }
}

/// One rung's verdict: which rung, what it chose, and its confidence.
///
/// `confidence` is the rung's own readout — for L1 the cosine similarity of
/// the best seed-example match (a probe-like score, not verbalized), for L2
/// the LLM's reported confidence. No threshold is applied at this layer.
///
/// `signal` is the optional concrete routing signal the rung resolved. When
/// it names a real `INTENT_SIGNALS` signal, [`resolve_agent`] prefers it over
/// the category default — that is what makes all 13 registered agents
/// reachable from every rung (T169). Embedding-derived signals come from
/// [`SEED_INTENT_EXAMPLES`]; LLM-derived signals survive
/// `intent::parse_classification`'s validation, so a hallucinated signal is
/// dropped before it reaches here.
#[derive(Debug, Clone)]
pub struct DecisionOutcome<C> {
    pub rung: DecisionRung,
    pub choice: C,
    pub confidence: f32,
    pub signal: Option<String>,
}

/// Why a rung could not produce a verdict. Callers fail open (warn + skip).
#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error("embedder unavailable: {0}")]
    EmbedderUnavailable(String),
    #[error("no seed examples could be embedded")]
    NoExamples,
    #[error("decision rung state poisoned")]
    Poisoned,
    #[error("local classifier unavailable after retry: {0}")]
    ClassifierUnavailable(String),
}

/// A decision rung — interchangeable implementations of one decision.
///
/// `Ok(None)` means the rung abstains (e.g. a keyword miss on L0); `Err`
/// means the rung is unavailable and the caller should fail open.
#[async_trait]
pub trait DecisionFn<C>: Send + Sync {
    async fn decide(&self, query: &str) -> Result<Option<DecisionOutcome<C>>, DecisionError>;
}

/// Embedder abstraction for the L1 rung — injectable so tests use a
/// deterministic fake without ONNX. Object-safe async trait per repo
/// convention (`#[async_trait]`, owned payloads).
#[async_trait]
pub trait TextEmbedder: Send + Sync {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
}

/// Adapter over the existing local embedding path
/// (`zen_vault::compute_embeddings_for_text`) — the local ONNX/ollama route,
/// never a cloud call (`Sensitivity::Private` locality is a hard rule).
pub struct VaultTextEmbedder;

#[async_trait]
impl TextEmbedder for VaultTextEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let text = text.to_string();
        // compute_embeddings_for_text is a blocking sync call (ONNX/ollama
        // inference) — run it off the async runtime (spawn_blocking guard,
        // same hazard class as intent::llm_classify).
        tokio::task::spawn_blocking(move || zen_vault::compute_embeddings_for_text(&text))
            .await
            .map_err(|e| anyhow::anyhow!("embed join: {e}"))?
    }
}

#[async_trait]
impl TextEmbedder for Arc<dyn TextEmbedder> {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.as_ref().embed(text).await
    }
}

/// L0 rung: the keyword/INTENT_SIGNALS fast path, adapted over
/// [`crate::intent::keyword_route`] — not forked or reimplemented. Abstains
/// (`Ok(None)`) on a keyword miss.
pub struct KeywordRung;

#[async_trait]
impl DecisionFn<IntentCategory> for KeywordRung {
    async fn decide(
        &self,
        query: &str,
    ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
        Ok(
            crate::intent::keyword_route(query).map(|intent| DecisionOutcome {
                rung: DecisionRung::L0,
                choice: intent.category,
                confidence: intent.confidence,
                signal: Some(intent.signal),
            }),
        )
    }
}

/// L2 rung: the existing LLM classification path, adapted over
/// [`crate::intent::classify`] — not forked or reimplemented.
pub struct LlmRung {
    router: DefaultRouter,
    sensitivity: Sensitivity,
}

impl LlmRung {
    pub fn new(router: &DefaultRouter, sensitivity: Sensitivity) -> Self {
        Self {
            router: router.clone(),
            sensitivity,
        }
    }
}

#[async_trait]
impl DecisionFn<IntentCategory> for LlmRung {
    async fn decide(
        &self,
        query: &str,
    ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
        let (intent, _telemetry) =
            crate::intent::classify(&self.router, query, self.sensitivity).await;
        Ok(Some(DecisionOutcome {
            rung: DecisionRung::L2,
            choice: intent.category,
            confidence: intent.confidence,
            signal: Some(intent.signal)
                .filter(|signal| crate::intent::agent_for_signal(signal).is_some()),
        }))
    }
}

/// What a seed example maps onto.
#[derive(Debug, Clone, Copy)]
pub enum SeedTarget {
    /// A concrete routing signal. Its category is derived through the shared
    /// `keyword_signal_category` mapping, so L0 and L1 can never disagree
    /// about which category a signal belongs to.
    Signal(&'static str),
    /// A bare category for intents with no signal (System, Conversation).
    Category(IntentCategory),
}

impl SeedTarget {
    fn category(&self) -> IntentCategory {
        match self {
            Self::Signal(signal) => crate::intent::keyword_signal_category(signal).0,
            Self::Category(category) => *category,
        }
    }

    fn signal(&self) -> Option<String> {
        match self {
            Self::Signal(signal) => Some((*signal).to_string()),
            Self::Category(_) => None,
        }
    }
}

/// Seed intent-example table (T168). One example utterance per routing signal,
/// plus bare-category examples for the signal-less categories, matched by
/// cosine similarity over embeddings.
///
/// Every `INTENT_SIGNALS` signal has an example here — that coverage is what
/// makes the 12 signal-addressable agents reachable through L1 (Sisyphus
/// arrives via the System/Conversation category defaults), and a test pins it.
/// This is a **SEED**: replace with real examples distilled from labeled
/// decision data once T173's labels accumulate. No threshold is applied here;
/// the router returns the argmax match, and the ladder's calibrated τ decides.
pub const SEED_INTENT_EXAMPLES: &[(&str, SeedTarget)] = &[
    // Query — read, analyse, organise, decide.
    ("search my notes for tokio", SeedTarget::Signal("research")),
    ("what do I know about rust", SeedTarget::Signal("research")),
    (
        "analyse the tradeoffs of this architecture",
        SeedTarget::Signal("deep-analysis"),
    ),
    (
        "organize my wiki pages and catalogue them",
        SeedTarget::Signal("knowledge-org"),
    ),
    (
        "plan the roadmap for next quarter",
        SeedTarget::Signal("planning"),
    ),
    (
        "review the quality of that change",
        SeedTarget::Signal("review-quality"),
    ),
    (
        "assess the gaps in this assumption",
        SeedTarget::Signal("gap-assessment"),
    ),
    (
        "should we prioritise this",
        SeedTarget::Signal("value-alignment"),
    ),
    // Action — create, modify, execute.
    ("implement a new function", SeedTarget::Signal("coder")),
    ("refactor this code", SeedTarget::Signal("coder")),
    ("debug the failing test", SeedTarget::Signal("coder")),
    (
        "convert this file to pdf",
        SeedTarget::Signal("format-convert"),
    ),
    (
        "batch automate the routine",
        SeedTarget::Signal("batch-automation"),
    ),
    (
        "merge and compile the wiki",
        SeedTarget::Signal("consolidate-pipeline"),
    ),
    (
        "generate a chart of the results",
        SeedTarget::Signal("visual"),
    ),
    // System — manage configuration or services.
    (
        "check the gateway status",
        SeedTarget::Category(IntentCategory::System),
    ),
    (
        "restart the daemon",
        SeedTarget::Category(IntentCategory::System),
    ),
    (
        "show my configuration",
        SeedTarget::Category(IntentCategory::System),
    ),
    // Conversation — chat, help, clarification.
    (
        "tell me a joke",
        SeedTarget::Category(IntentCategory::Conversation),
    ),
    (
        "what can you do",
        SeedTarget::Category(IntentCategory::Conversation),
    ),
    (
        "just chatting",
        SeedTarget::Category(IntentCategory::Conversation),
    ),
];

struct Example {
    category: IntentCategory,
    signal: Option<String>,
    embedding: Vec<f32>,
}

/// L1 rung: cosine match of the query against the seed example table.
///
/// Example embeddings are computed lazily on first use and cached, so the
/// per-turn cost is one query embedding. The router never abstains: the
/// argmax match is always returned, with its cosine similarity as the
/// (uncalibrated) confidence — calibration is T173's job, not this layer's.
pub struct EmbeddingIntentRouter<E> {
    embedder: E,
    examples: Mutex<Option<Arc<Vec<Example>>>>,
}

impl<E: TextEmbedder> EmbeddingIntentRouter<E> {
    pub fn new(embedder: E) -> Self {
        Self {
            embedder,
            examples: Mutex::new(None),
        }
    }

    async fn examples(&self) -> Result<Arc<Vec<Example>>, DecisionError> {
        let cached = self
            .examples
            .lock()
            .map_err(|_| DecisionError::Poisoned)?
            .clone();
        if let Some(examples) = cached {
            return Ok(examples);
        }
        let mut computed = Vec::new();
        for &(text, target) in SEED_INTENT_EXAMPLES {
            match self.embedder.embed(text).await {
                Ok(embedding) => computed.push(Example {
                    category: target.category(),
                    signal: target.signal(),
                    embedding,
                }),
                Err(e) => warn!(
                    error = %e,
                    example = text,
                    "L1 seed example embedding failed; skipping"
                ),
            }
        }
        if computed.is_empty() {
            return Err(DecisionError::NoExamples);
        }
        let arc = Arc::new(computed);
        *self.examples.lock().map_err(|_| DecisionError::Poisoned)? = Some(arc.clone());
        Ok(arc)
    }
}

#[async_trait]
impl<E: TextEmbedder> DecisionFn<IntentCategory> for EmbeddingIntentRouter<E> {
    async fn decide(
        &self,
        query: &str,
    ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
        let examples = self.examples().await?;
        let query_embedding = self
            .embedder
            .embed(query)
            .await
            .map_err(|e| DecisionError::EmbedderUnavailable(e.to_string()))?;
        let mut best: Option<(f32, &Example)> = None;
        for example in examples.iter() {
            let similarity = cosine_similarity(&query_embedding, &example.embedding);
            if best.is_none_or(|(best_sim, _)| similarity > best_sim) {
                best = Some((similarity, example));
            }
        }
        let Some((confidence, example)) = best else {
            return Err(DecisionError::NoExamples);
        };
        Ok(Some(DecisionOutcome {
            rung: DecisionRung::L1,
            choice: example.category,
            confidence,
            signal: example.signal.clone(),
        }))
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    let denom = (norm_a.sqrt() * norm_b.sqrt()).max(f32::EPSILON);
    dot / denom
}

/// Calibrated operating points for the decision ladder (written by T173's
/// harness, consumed by T169's gates).
///
/// V13-A.3 forbids inventing a threshold: every gate in this module reads its
/// τ from here or from an explicit config override, and `None` — the default,
/// and the only state until labeled data exists — leaves the gate closed and
/// the pre-T169 behaviour exactly intact. The artefact is produced once
/// `logs/decision-audit/labels.jsonl` has adjudicated entries.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DecisionThresholds {
    /// Intent L1 gate: accept the L1 rung's verdict at or above this score,
    /// skipping the L2 LLM classification call.
    pub intent_l1: Option<f32>,
    /// Semantic-review escalation gate: escalate to the frontier reviewer only
    /// below this local-judge confidence.
    pub review_escalate: Option<f32>,
    /// "Is this turn a user correction?" classifier gate.
    pub correction_l1: Option<f32>,
    /// "Is this passage a citation?" classifier gate.
    pub citation_l1: Option<f32>,
}

impl DecisionThresholds {
    /// Load calibrated thresholds, failing open to "every gate closed".
    ///
    /// A missing, unreadable, corrupt or out-of-range artefact must never block
    /// or mis-route a turn: it degrades per field to `None` with a warning,
    /// which is exactly the uncalibrated state the ladder tolerates by design.
    pub fn load(path: &Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        %error,
                        path = %path.display(),
                        "decision thresholds unreadable; every gate stays closed"
                    );
                }
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(parsed) => Self {
                intent_l1: validate_threshold(parsed.intent_l1, "intent_l1"),
                review_escalate: validate_threshold(parsed.review_escalate, "review_escalate"),
                correction_l1: validate_threshold(parsed.correction_l1, "correction_l1"),
                citation_l1: validate_threshold(parsed.citation_l1, "citation_l1"),
            },
            Err(error) => {
                warn!(
                    %error,
                    path = %path.display(),
                    "decision thresholds corrupt; every gate stays closed"
                );
                Self::default()
            }
        }
    }
}

/// A threshold is usable only inside `0.0..=1.0`; anything else closes the
/// gate rather than silently mis-gating.
fn validate_threshold(value: Option<f32>, field: &str) -> Option<f32> {
    match value {
        Some(value) if (0.0..=1.0).contains(&value) => Some(value),
        Some(value) => {
            warn!(
                field,
                value, "decision threshold outside 0.0..=1.0; gate stays closed"
            );
            None
        }
        None => None,
    }
}

/// `<ZEN_HOME>/logs/decision-audit/thresholds.json` — where T173's harness
/// writes calibrated operating points.
pub fn thresholds_path(paths: &ZenPaths) -> std::path::PathBuf {
    paths.logs().join("decision-audit").join("thresholds.json")
}

/// Resolve the agent for a rung's verdict: a concrete signal wins over the
/// category default (T169). This is what makes all 13 registered agents
/// reachable from every rung, retiring the old 3-category-default ceiling.
pub fn resolve_agent(category: IntentCategory, signal: Option<&str>) -> String {
    signal
        .and_then(crate::intent::agent_for_signal)
        .unwrap_or_else(|| category.default_agent())
        .to_string()
}

/// Attempts a typed classification round makes before the rung gives up.
const CLASSIFIER_RETRIES: usize = 2;

/// L1 rung: the typed-output local classifier.
///
/// Genuinely local-only by construction — it abstains when no local model is
/// reachable, and otherwise routes through the sensitivity enforcement in
/// local mode, so it cannot reach a cloud provider. The typed contract is
/// enforced by parse-and-validate (the provider layer exposes no grammar
/// passthrough) with one retry, after which the ladder falls through to L2.
pub struct LocalClassifierRung {
    router: DefaultRouter,
}

impl LocalClassifierRung {
    pub fn new(router: &DefaultRouter) -> Self {
        Self {
            router: router.clone(),
        }
    }
}

#[async_trait]
impl DecisionFn<IntentCategory> for LocalClassifierRung {
    async fn decide(
        &self,
        query: &str,
    ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
        if !zen_provider::is_local_llm_available(&self.router) {
            return Ok(None);
        }
        let mut last_error = String::new();
        for _ in 0..CLASSIFIER_RETRIES {
            // Sensitivity::Private is the local-only enforcement path, which is
            // precisely the L1 rule ("no cloud call at L1") expressed in the
            // router's own vocabulary.
            match crate::intent::classify_typed(&self.router, query, Sensitivity::Private).await {
                Ok(Some((category, confidence, signal))) => {
                    return Ok(Some(DecisionOutcome {
                        rung: DecisionRung::L1,
                        choice: category,
                        confidence,
                        signal,
                    }));
                }
                Ok(None) => return Ok(None),
                Err(error) => last_error = error,
            }
        }
        Err(DecisionError::ClassifierUnavailable(last_error))
    }
}

/// Which rung served a turn, and the calibrated gate it cleared.
#[derive(Debug, Clone)]
pub struct LadderTrace {
    pub rung: &'static str,
    pub gate: Option<f32>,
    pub confidence: f32,
    pub latency_ms: u64,
}

/// One turn's ladder outcome. `trace` is `None` when L1 did not serve the
/// turn, i.e. the L2/keyword path ran exactly as it did before T169.
#[derive(Debug, Clone)]
pub struct LadderDecision {
    pub intent: Intent,
    pub telemetry: LlmTelemetry,
    pub trace: Option<LadderTrace>,
}

/// Classify one user turn through the three-tier ladder (T169).
///
/// With **no calibrated threshold** this delegates to [`crate::intent::classify`]
/// and returns its result untouched — the pre-T169 behaviour, byte-for-byte.
/// With a threshold it tries L1 first (embedding router, then the local
/// classifier) and accepts a verdict at or above τ, skipping the LLM
/// classification call; anything below τ, abstained, or broken falls through
/// to the same L2 path. L1 failures are always fail-open: no turn is lost to a
/// broken rung.
pub async fn classify_intent(
    paths: Option<&ZenPaths>,
    router: &DefaultRouter,
    query: &str,
    sensitivity: Sensitivity,
    l1_embedding: Option<&dyn DecisionFn<IntentCategory>>,
) -> LadderDecision {
    let Some(threshold) = resolve_intent_threshold(paths) else {
        let (intent, telemetry) = crate::intent::classify(router, query, sensitivity).await;
        return LadderDecision {
            intent,
            telemetry,
            trace: None,
        };
    };

    let start = std::time::Instant::now();
    let embedding: &dyn DecisionFn<IntentCategory> = match l1_embedding {
        Some(rung) => rung,
        None => default_l1_router(),
    };
    if let Some(outcome) = try_rung(embedding, query, threshold).await {
        return served_by_l1(outcome, start, threshold);
    }
    let local = LocalClassifierRung::new(router);
    if let Some(outcome) = try_rung(&local, query, threshold).await {
        return served_by_l1(outcome, start, threshold);
    }

    let (intent, telemetry) = crate::intent::classify(router, query, sensitivity).await;
    LadderDecision {
        intent,
        telemetry,
        trace: None,
    }
}

/// Config override first (so a user can pin or replace a calibrated value),
/// then the calibration artefact. `None` keeps every L1 gate closed.
fn resolve_intent_threshold(paths: Option<&ZenPaths>) -> Option<f32> {
    let configured = zen_core::config::load_config()
        .ok()
        .and_then(|config| config.agentic.intent.l1_threshold());
    configured.or_else(|| {
        paths.and_then(|paths| DecisionThresholds::load(&thresholds_path(paths)).intent_l1)
    })
}

/// Resolve the review escalation threshold (T170) — config override first,
/// then the calibration artefact at the detected `ZEN_HOME`.
///
/// `None` keeps the de-anchored local judge phase off, which preserves the
/// pre-T170 frontier-only review path exactly.
pub fn resolve_review_escalate_threshold() -> Option<f32> {
    let configured = zen_core::config::load_config()
        .ok()
        .and_then(|config| config.agentic.review.escalate_threshold());
    configured.or_else(|| {
        ZenPaths::detect()
            .ok()
            .and_then(|paths| DecisionThresholds::load(&thresholds_path(&paths)).review_escalate)
    })
}

/// Run one rung, returning its verdict only when it clears the calibrated
/// threshold. A rung that abstains, is below τ, or errors simply yields
/// `None` — the ladder falls through; a turn is never failed by L1.
async fn try_rung(
    rung: &dyn DecisionFn<IntentCategory>,
    query: &str,
    threshold: f32,
) -> Option<DecisionOutcome<IntentCategory>> {
    match rung.decide(query).await {
        Ok(Some(outcome)) if outcome.confidence >= threshold => Some(outcome),
        Ok(Some(outcome)) => {
            tracing::debug!(
                rung = outcome.rung.as_str(),
                confidence = outcome.confidence,
                threshold,
                "L1 rung below calibrated threshold; falling through to L2"
            );
            None
        }
        Ok(None) => None,
        Err(error) => {
            warn!(%error, "L1 rung unavailable; falling through to L2");
            None
        }
    }
}

fn served_by_l1(
    outcome: DecisionOutcome<IntentCategory>,
    start: std::time::Instant,
    threshold: f32,
) -> LadderDecision {
    let latency_ms = start.elapsed().as_millis() as u64;
    let intent = Intent::from_ladder(
        outcome.choice,
        outcome.signal.as_deref(),
        outcome.confidence,
        IntentSource::L1,
    );
    LadderDecision {
        intent,
        telemetry: LlmTelemetry {
            outcome: LlmOutcome::L1Resolved,
            elapsed_ms: 0,
        },
        trace: Some(LadderTrace {
            rung: outcome.rung.as_str(),
            gate: Some(threshold),
            confidence: outcome.confidence,
            latency_ms,
        }),
    }
}

/// Record one `loop.decision` line for a ladder-served turn.
///
/// Emitted only when L1 actually served: the L2/keyword path is already
/// covered by `loop.turn.review`'s intent fields, and duplicating it would
/// double-count the routing distribution `bin/orchestration-stats` derives.
/// The `rung` + `latency_ms` pair is what makes the V13-A.3 baselines
/// measurable (L1-served rate, intent p95).
pub fn record_ladder_decision(paths: &ZenPaths, session_id: &str, decision: &LadderDecision) {
    let Some(trace) = decision.trace.as_ref() else {
        return;
    };
    let entry = serde_json::json!({
        "kind": "loop.decision",
        "decision": "intent",
        // `decision_kind` is what decision_audit::record_from_line reads; the
        // ladder's L1 sample is selectable as (kind = "intent", rung = "L1")
        // only because this field is present.
        "decision_kind": "intent",
        "session_id": session_id,
        "rung": trace.rung,
        "gate": trace.gate,
        "gate_fired": trace.gate.is_some_and(|gate| trace.confidence >= gate),
        "confidence": audit_score(trace.confidence),
        "latency_ms": trace.latency_ms,
        "choice": decision.intent.category.as_str(),
        "agent": decision.intent.agent,
        "signal": decision.intent.signal,
    });
    append_decision_audit(paths, &entry);
}

/// A binary decision the L1 classifier serves (T171).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryDecision {
    /// Is this user turn correcting the assistant's prior output? Replaces
    /// the T161 `CORRECTION_MARKERS` substring rule (FR-034 anti-poisoning).
    Correction,
    /// Does this response actually use/cite this retrieved note? Replaces the
    /// T161 body-fingerprint containment rule (FR-034 `downstream_citations`).
    Citation,
}

impl BinaryDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Correction => "correction",
            Self::Citation => "citation",
        }
    }
}

/// The binary classifier's prompt. Pure, so the contract it states is
/// testable without a provider. `input` is composed by the caller (for
/// [`BinaryDecision::Citation`] it carries the labelled CONTENT/RESPONSE
/// sections).
pub fn binary_prompt(decision: BinaryDecision, input: &str) -> String {
    let instruction = match decision {
        BinaryDecision::Correction => {
            "Decide whether the user's message is CORRECTING the assistant's previous output \
             (pointing out an error, contradiction, or wrong fact), as opposed to asking a new \
             question, giving a new instruction, or chatting. A question that merely mentions an \
             error - for example \"explain why the previous answer was wrong\" - is NOT a correction."
        }
        BinaryDecision::Citation => {
            "The input below carries a CONTENT section and a RESPONSE section. Decide whether the \
             RESPONSE actually uses or cites the CONTENT. Merely echoing a heading, a frontmatter \
             field, or a title is NOT a citation."
        }
    };
    format!(
        "{instruction}\nInput:\n{input}\nRespond with ONLY a JSON object: {{\"answer\": true|false, \"confidence\": <0.0-1.0>}}"
    )
}

/// Parse the binary classifier's typed reply. Accepts a JSON boolean or the
/// strings `true/false`/`yes/no`; anything else is unparsable (the caller
/// retries, then fails open to the heuristic).
pub fn parse_binary(reply: &str) -> Option<(bool, f32)> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    if end < start {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
    let answer = match value.get("answer")? {
        serde_json::Value::Bool(answer) => *answer,
        serde_json::Value::String(raw) => match raw.trim().to_lowercase().as_str() {
            "true" | "yes" | "y" => true,
            "false" | "no" | "n" => false,
            _ => return None,
        },
        _ => return None,
    };
    let confidence = value.get("confidence")?.as_f64()? as f32;
    Some((answer, confidence.clamp(0.0, 1.0)))
}

/// L1 rung: the local binary classifier.
///
/// Same locality contract as [`LocalClassifierRung`] — abstains when no local
/// model is reachable, otherwise routes through local-mode sensitivity
/// enforcement, so it cannot reach a cloud provider. Typed contract enforced
/// by parse-and-validate with one retry.
pub struct LocalBinaryClassifier {
    router: DefaultRouter,
    decision: BinaryDecision,
}

impl LocalBinaryClassifier {
    pub fn new(router: &DefaultRouter, decision: BinaryDecision) -> Self {
        Self {
            router: router.clone(),
            decision,
        }
    }
}

#[async_trait]
impl DecisionFn<bool> for LocalBinaryClassifier {
    async fn decide(&self, input: &str) -> Result<Option<DecisionOutcome<bool>>, DecisionError> {
        if !zen_provider::is_local_llm_available(&self.router) {
            return Ok(None);
        }
        let mut last_error = String::new();
        for _ in 0..CLASSIFIER_RETRIES {
            let reply = crate::intent::complete_prompt(
                &self.router,
                &binary_prompt(self.decision, input),
                Sensitivity::Private,
                64,
            )
            .await;
            match reply {
                Ok(reply) => match parse_binary(&reply) {
                    Some((answer, confidence)) => {
                        return Ok(Some(DecisionOutcome {
                            rung: DecisionRung::L1,
                            choice: answer,
                            confidence,
                            signal: None,
                        }));
                    }
                    None => last_error = format!("unparsable binary reply: {reply}"),
                },
                Err(error) => last_error = error,
            }
        }
        Err(DecisionError::ClassifierUnavailable(last_error))
    }
}

/// Apply the calibrated gate to a binary decision (T171).
///
/// `heuristic` is T161's L0 verdict and stays authoritative whenever no τ is
/// calibrated: the substring/fingerprint rules are not retired until a
/// classifier can be measured against labeled contradictions, and that is
/// what V13-A.3's "no invented threshold" rule requires. With τ, a confident
/// classifier verdict wins; below τ, on abstain, or on error the heuristic
/// decides — so a broken classifier never silently rewrites the reward
/// bookkeeping.
pub async fn classify_binary(
    threshold: Option<f32>,
    heuristic: bool,
    l1: &dyn DecisionFn<bool>,
    input: &str,
) -> bool {
    let Some(threshold) = threshold else {
        return heuristic;
    };
    match l1.decide(input).await {
        Ok(Some(outcome)) if outcome.confidence >= threshold => outcome.choice,
        Ok(Some(outcome)) => {
            tracing::debug!(
                decision = outcome.rung.as_str(),
                confidence = outcome.confidence,
                threshold,
                "binary classifier below the calibrated threshold; heuristic decides"
            );
            heuristic
        }
        Ok(None) => heuristic,
        Err(error) => {
            warn!(%error, "binary classifier unavailable; heuristic decides");
            heuristic
        }
    }
}

/// Resolve a binary decision's threshold — config override first, then the
/// calibration artefact. `None` keeps T161's heuristic authoritative.
pub fn resolve_binary_threshold(decision: BinaryDecision) -> Option<f32> {
    let configured = zen_core::config::load_config()
        .ok()
        .and_then(|config| match decision {
            BinaryDecision::Correction => config.agentic.classifiers.correction_threshold(),
            BinaryDecision::Citation => config.agentic.classifiers.citation_threshold(),
        });
    configured.or_else(|| {
        ZenPaths::detect().ok().and_then(|paths| {
            let thresholds = DecisionThresholds::load(&thresholds_path(&paths));
            match decision {
                BinaryDecision::Correction => thresholds.correction_l1,
                BinaryDecision::Citation => thresholds.citation_l1,
            }
        })
    })
}

/// T168: shadow-mode L1 observation at the intent decision point. Runs the
/// L1 rung after the production decision and appends a `loop.decision` audit
/// line. Never affects the production decision — the caller returns the
/// production intent unchanged whether shadow is on or off.
///
/// `l1` is injectable for tests; `None` uses the process-wide default router
/// (Vault embedder, example embeddings cached across turns).
pub async fn run_intent_shadow(
    paths: &ZenPaths,
    session_id: &str,
    query: &str,
    production: &Intent,
    l1: Option<&dyn DecisionFn<IntentCategory>>,
) {
    let start = std::time::Instant::now();
    let verdict = match l1 {
        Some(rung) => rung.decide(query).await,
        None => default_l1_router().decide(query).await,
    };
    let latency_ms = start.elapsed().as_millis() as u64;
    let entry = match verdict {
        Ok(Some(outcome)) => serde_json::json!({
            "kind": "loop.decision",
            "decision": "intent",
            "session_id": session_id,
            "production_rung": production_rung(production.source),
            "production_source": format!("{:?}", production.source),
            "production_choice": production.category.as_str(),
            "l1_rung": outcome.rung.as_str(),
            "l1_choice": outcome.choice.as_str(),
            "l1_confidence": audit_score(outcome.confidence),
            "agree": outcome.choice == production.category,
            "l1_latency_ms": latency_ms,
        }),
        Ok(None) => {
            warn!("L1 shadow rung abstained; skipping shadow observation");
            return;
        }
        Err(e) => {
            warn!(error = %e, "L1 shadow rung unavailable; skipping shadow observation");
            return;
        }
    };
    append_decision_audit(paths, &entry);
}

/// Map the production intent's provenance onto the ladder rung that produced
/// it. The Conversation fallback is deterministic Rust, so it counts as L0.
fn production_rung(source: IntentSource) -> &'static str {
    match source {
        IntentSource::L1 => DecisionRung::L1.as_str(),
        IntentSource::Llm => DecisionRung::L2.as_str(),
        IntentSource::Keyword | IntentSource::Fallback => DecisionRung::L0.as_str(),
    }
}

fn default_l1_router() -> &'static EmbeddingIntentRouter<Arc<dyn TextEmbedder>> {
    static ROUTER: OnceLock<EmbeddingIntentRouter<Arc<dyn TextEmbedder>>> = OnceLock::new();
    ROUTER.get_or_init(|| EmbeddingIntentRouter::new(Arc::new(VaultTextEmbedder)))
}

/// Round a score for audit emission.
///
/// A bare f32 widened to f64 serializes as 0.6000000238418579, which makes the
/// human-audited log unreadable and disagrees with the rounded value the
/// calibration artefact stores. Four decimals is more than a confidence gate
/// can distinguish.
pub(crate) fn audit_score(value: f32) -> f64 {
    (f64::from(value) * 10_000.0).round() / 10_000.0
}

/// The configured decision excerpt, or `None` when the user has not opted in.
///
/// `[agentic.audit] decision_excerpt_chars` defaults to 0 = off, because this
/// writes (bounded) user input into a local log file — a privacy decision that
/// belongs to the user, never a silent default. Convenience wrapper that loads
/// the config once; the clamping, character-boundary truncation and newline
/// flattening live in [`zen_core::config::AuditConfig::excerpt`] so those
/// rules have exactly one implementation.
pub fn decision_excerpt(input: &str) -> Option<String> {
    zen_core::config::load_config()
        .ok()
        .and_then(|config| config.agentic.audit.excerpt(input))
}

/// Append one `loop.decision` line to `<logs>/audit.jsonl` — same file and
/// style as the `loop.turn.review` line (additive; existing fields untouched).
pub(crate) fn append_decision_audit(paths: &ZenPaths, entry: &serde_json::Value) {
    let log_path = paths.logs().join("audit.jsonl");
    if let Some(parent) = log_path.parent()
        && std::fs::create_dir_all(parent).is_ok()
    {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{entry}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{Acl, IntentSource};

    /// Deterministic bag-of-words embedder: 64-dim word-hash buckets,
    /// L2-normalized. Similar texts share buckets → meaningful cosine.
    struct FakeEmbedder;

    #[async_trait]
    impl TextEmbedder for FakeEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let mut v = vec![0.0f32; 64];
            for word in text.split_whitespace() {
                let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                for b in word.to_lowercase().bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
                v[(h % 64) as usize] += 1.0;
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-8 {
                for x in v.iter_mut() {
                    *x /= norm;
                }
            }
            Ok(v)
        }
    }

    /// Embedder that always errors — exercises the fail-open path.
    struct FailingEmbedder;

    #[async_trait]
    impl TextEmbedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Err(anyhow::anyhow!("embedder offline"))
        }
    }

    fn test_paths() -> ZenPaths {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        // Keep the tempdir alive for the test's duration.
        std::mem::forget(dir);
        ZenPaths::for_testing(root)
    }

    fn production_intent(category: IntentCategory, source: IntentSource) -> Intent {
        let acl = match category {
            IntentCategory::Query => Acl::ReadOnly,
            IntentCategory::Action => Acl::Write,
            IntentCategory::System => Acl::Admin,
            IntentCategory::Conversation => Acl::None,
        };
        Intent {
            category,
            agent: "Sisyphus".to_string(),
            signal: "default".to_string(),
            acl,
            confidence: 0.8,
            source,
        }
    }

    #[tokio::test]
    async fn router_ranks_matching_example_above_non_matching() {
        let router = EmbeddingIntentRouter::new(FakeEmbedder);
        let outcome = router
            .decide("please refactor this function")
            .await
            .expect("decide")
            .expect("L1 never abstains");
        assert_eq!(outcome.rung, DecisionRung::L1);
        assert_eq!(outcome.choice, IntentCategory::Action);
        assert!(
            outcome.confidence > 0.0,
            "shares words with the Action example"
        );

        let query = router
            .decide("search my notes for tokio")
            .await
            .expect("decide")
            .expect("L1 never abstains");
        assert_eq!(query.choice, IntentCategory::Query);
        assert!(
            (query.confidence - 1.0).abs() < 1e-6,
            "exact seed match scores 1.0"
        );
    }

    #[tokio::test]
    async fn shadow_comparison_emits_expected_fields() {
        let paths = test_paths();
        let production = production_intent(IntentCategory::Query, IntentSource::Llm);
        let l1 = EmbeddingIntentRouter::new(FakeEmbedder);
        run_intent_shadow(
            &paths,
            "session-1",
            "search my notes for tokio",
            &production,
            Some(&l1),
        )
        .await;

        let audit = std::fs::read_to_string(paths.logs().join("audit.jsonl")).expect("audit file");
        let line = audit.lines().next().expect("one audit line");
        let entry: serde_json::Value = serde_json::from_str(line).expect("valid json");
        assert_eq!(entry["kind"], "loop.decision");
        assert_eq!(entry["decision"], "intent");
        assert_eq!(entry["session_id"], "session-1");
        assert_eq!(entry["production_rung"], "L2");
        assert_eq!(entry["production_source"], "Llm");
        assert_eq!(entry["production_choice"], "Query");
        assert_eq!(entry["l1_rung"], "L1");
        assert_eq!(entry["l1_choice"], "Query");
        assert!(entry["l1_confidence"].as_f64().unwrap() > 0.9);
        assert_eq!(entry["agree"], true);
        assert!(entry["l1_latency_ms"].as_u64().is_some());
    }

    #[tokio::test]
    async fn shadow_observation_does_not_mutate_production_intent() {
        let paths = test_paths();
        let production = production_intent(IntentCategory::Action, IntentSource::Keyword);
        let before = production.clone();
        let l1 = EmbeddingIntentRouter::new(FakeEmbedder);
        run_intent_shadow(
            &paths,
            "session-2",
            "please refactor this function",
            &production,
            Some(&l1),
        )
        .await;
        assert_eq!(production.category, before.category);
        assert_eq!(production.agent, before.agent);
        assert_eq!(production.signal, before.signal);
        assert_eq!(production.acl, before.acl);
        assert_eq!(production.confidence, before.confidence);
        assert_eq!(production.source, before.source);
    }

    #[tokio::test]
    async fn embedder_error_leaves_production_path_intact() {
        let paths = test_paths();
        let production = production_intent(IntentCategory::Conversation, IntentSource::Fallback);
        let before = production.clone();
        let l1 = EmbeddingIntentRouter::new(FailingEmbedder);
        // Must not panic; must not write an audit line; production unchanged.
        run_intent_shadow(
            &paths,
            "session-3",
            "tell me a joke",
            &production,
            Some(&l1),
        )
        .await;
        assert_eq!(production.category, before.category);
        assert_eq!(production.source, before.source);
        assert!(
            !paths.logs().join("audit.jsonl").exists(),
            "no audit line on embedder failure"
        );
    }

    #[tokio::test]
    async fn keyword_rung_abstains_on_miss_and_routes_on_hit() {
        let rung = KeywordRung;
        let miss = rung.decide("tell me a joke").await.expect("no error");
        assert!(miss.is_none(), "keyword miss abstains");

        let hit = rung
            .decide("please refactor this function")
            .await
            .expect("no error")
            .expect("keyword hit");
        assert_eq!(hit.rung, DecisionRung::L0);
        assert_eq!(hit.choice, IntentCategory::Action);
        assert_eq!(hit.confidence, 0.8);
    }

    #[tokio::test]
    async fn llm_rung_adapts_existing_classify_path() {
        let router = zen_provider::DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: None,
            ..Default::default()
        });
        let rung = LlmRung::new(&router, Sensitivity::Public);
        let outcome = rung
            .decide("please refactor this function")
            .await
            .expect("no error")
            .expect("L2 never abstains");
        assert_eq!(outcome.rung, DecisionRung::L2);
        assert_eq!(outcome.choice, IntentCategory::Action);
    }

    #[test]
    fn production_rung_maps_sources_onto_ladder() {
        assert_eq!(production_rung(IntentSource::Llm), "L2");
        assert_eq!(production_rung(IntentSource::Keyword), "L0");
        assert_eq!(production_rung(IntentSource::Fallback), "L0");
    }

    #[test]
    fn decision_rung_as_str_roundtrips() {
        assert_eq!(DecisionRung::L0.as_str(), "L0");
        assert_eq!(DecisionRung::L1.as_str(), "L1");
        assert_eq!(DecisionRung::L2.as_str(), "L2");
    }

    /// Rung that returns a fixed verdict — lets the ladder tests control the
    /// score without an embedder, an LLM, or a live provider.
    struct FixedRung {
        outcome: Option<DecisionOutcome<IntentCategory>>,
    }

    #[async_trait]
    impl DecisionFn<IntentCategory> for FixedRung {
        async fn decide(
            &self,
            _query: &str,
        ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
            Ok(self.outcome.clone())
        }
    }

    fn l1_outcome(confidence: f32, signal: Option<&str>) -> DecisionOutcome<IntentCategory> {
        DecisionOutcome {
            rung: DecisionRung::L1,
            choice: IntentCategory::Query,
            confidence,
            signal: signal.map(str::to_string),
        }
    }

    fn router_without_provider() -> DefaultRouter {
        DefaultRouter::new(zen_provider::LlmConfig {
            default_provider: None,
            ..Default::default()
        })
    }

    /// A ZenPaths whose calibration artefact carries the given intent gate.
    fn paths_with_intent_gate(gate: f32) -> ZenPaths {
        let paths = test_paths();
        let path = thresholds_path(&paths);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, format!(r#"{{"intent_l1": {gate}}}"#)).expect("write");
        paths
    }

    #[test]
    fn thresholds_missing_corrupt_and_out_of_range_close_every_gate() {
        let paths = test_paths();
        let path = thresholds_path(&paths);
        assert_eq!(
            DecisionThresholds::load(&path).intent_l1,
            None,
            "missing artefact ⇒ gate closed"
        );

        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{not json").expect("write");
        assert_eq!(
            DecisionThresholds::load(&path).intent_l1,
            None,
            "corrupt artefact ⇒ gate closed"
        );

        std::fs::write(&path, r#"{"intent_l1": 1.5, "review_escalate": 0.4}"#).expect("write");
        let loaded = DecisionThresholds::load(&path);
        assert_eq!(loaded.intent_l1, None, "out-of-range ⇒ gate closed");
        assert_eq!(
            loaded.review_escalate,
            Some(0.4),
            "one bad field must not close the others"
        );
    }

    #[tokio::test]
    async fn no_threshold_preserves_pre_t169_behaviour() {
        // No calibration artefact at all: the ladder must delegate, so a
        // high-confidence L1 verdict is ignored even though it would clear
        // any plausible threshold. This is the V13-A.3 regression guard.
        let paths = test_paths();
        let rung = FixedRung {
            outcome: Some(l1_outcome(1.0, Some("review-quality"))),
        };
        let decision = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "please refactor this function",
            Sensitivity::Public,
            Some(&rung),
        )
        .await;

        assert!(decision.trace.is_none(), "no τ ⇒ L1 never serves");
        assert_eq!(decision.intent.source, IntentSource::Keyword);
        assert_eq!(decision.intent.agent, "Hephaestus");
        assert_eq!(decision.telemetry.outcome, LlmOutcome::Skipped);
    }

    #[tokio::test]
    async fn calibrated_artefact_activates_the_l1_gate_without_an_llm_call() {
        let paths = paths_with_intent_gate(0.5);
        let rung = FixedRung {
            outcome: Some(l1_outcome(0.9, Some("review-quality"))),
        };
        let decision = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "review the quality of that change",
            Sensitivity::Public,
            Some(&rung),
        )
        .await;

        assert_eq!(decision.intent.source, IntentSource::L1);
        assert_eq!(
            decision.intent.agent, "Momus",
            "the L1 signal must beat the Query category default"
        );
        assert_eq!(decision.intent.category, IntentCategory::Query);
        assert_eq!(decision.intent.confidence, 0.9);
        assert_eq!(
            decision.telemetry.outcome,
            LlmOutcome::L1Resolved,
            "l1_resolved is the marker that the LLM call was skipped"
        );
        assert_eq!(decision.telemetry.elapsed_ms, 0);
        let trace = decision.trace.expect("trace records the serving rung");
        assert_eq!(trace.rung, "L1");
        assert_eq!(trace.gate, Some(0.5));
    }

    #[tokio::test]
    async fn below_threshold_and_broken_rungs_fall_through_to_l2() {
        let paths = paths_with_intent_gate(0.9);

        let low = FixedRung {
            outcome: Some(l1_outcome(0.4, None)),
        };
        let decision = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "please refactor this function",
            Sensitivity::Public,
            Some(&low),
        )
        .await;
        assert!(decision.trace.is_none(), "below τ ⇒ not served by L1");
        assert_eq!(decision.intent.source, IntentSource::Keyword);

        struct BrokenRung;
        #[async_trait]
        impl DecisionFn<IntentCategory> for BrokenRung {
            async fn decide(
                &self,
                _query: &str,
            ) -> Result<Option<DecisionOutcome<IntentCategory>>, DecisionError> {
                Err(DecisionError::EmbedderUnavailable("offline".to_string()))
            }
        }
        let decision = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "please refactor this function",
            Sensitivity::Public,
            Some(&BrokenRung),
        )
        .await;
        assert!(
            decision.trace.is_none(),
            "a broken rung must fail open, never fail the turn"
        );
        assert_eq!(decision.intent.source, IntentSource::Keyword);
        assert_eq!(decision.intent.agent, "Hephaestus");
    }

    #[tokio::test]
    async fn ladder_audit_line_is_emitted_only_when_l1_served() {
        let paths = paths_with_intent_gate(0.5);
        let rung = FixedRung {
            outcome: Some(l1_outcome(0.9, Some("coder"))),
        };

        let served = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "implement a new function",
            Sensitivity::Public,
            Some(&rung),
        )
        .await;
        record_ladder_decision(&paths, "session-ladder", &served);

        let audit = std::fs::read_to_string(paths.logs().join("audit.jsonl")).expect("audit file");
        let entry: serde_json::Value =
            serde_json::from_str(audit.lines().next().expect("one line")).expect("valid json");
        assert_eq!(entry["kind"], "loop.decision");
        assert_eq!(entry["rung"], "L1");
        assert_eq!(entry["gate_fired"], true);
        assert_eq!(entry["agent"], "Hephaestus");
        assert!(entry["latency_ms"].as_u64().is_some());

        // A turn L1 did not serve must not add a line (it is already covered by
        // loop.turn.review, and duplicating it would skew the routing stats).
        let not_served = classify_intent(
            Some(&paths),
            &router_without_provider(),
            "please refactor this function",
            Sensitivity::Public,
            Some(&FixedRung { outcome: None }),
        )
        .await;
        record_ladder_decision(&paths, "session-ladder", &not_served);

        let audit = std::fs::read_to_string(paths.logs().join("audit.jsonl")).expect("audit file");
        assert_eq!(
            audit.lines().count(),
            1,
            "only the L1-served turn is logged"
        );
    }

    #[test]
    fn seed_table_covers_every_routing_signal() {
        let covered: std::collections::BTreeSet<&str> = SEED_INTENT_EXAMPLES
            .iter()
            .filter_map(|(_, target)| match target {
                SeedTarget::Signal(signal) => Some(*signal),
                SeedTarget::Category(_) => None,
            })
            .collect();
        for (_, signal, _) in crate::intent::INTENT_SIGNALS {
            assert!(
                covered.contains(signal),
                "seed table must cover signal '{signal}' or that agent is unreachable via L1"
            );
        }
    }

    #[test]
    fn resolve_agent_prefers_signal_over_category_default() {
        assert_eq!(
            resolve_agent(IntentCategory::Query, Some("review-quality")),
            "Momus"
        );
        assert_eq!(resolve_agent(IntentCategory::Query, None), "Explore");
        assert_eq!(
            resolve_agent(IntentCategory::Action, Some("not-a-signal")),
            "Hephaestus",
            "an unknown signal must fall back, not route"
        );
    }

    /// Binary rung returning a fixed verdict (T171 tests).
    struct FixedBoolRung {
        outcome: Option<DecisionOutcome<bool>>,
    }

    #[async_trait]
    impl DecisionFn<bool> for FixedBoolRung {
        async fn decide(
            &self,
            _input: &str,
        ) -> Result<Option<DecisionOutcome<bool>>, DecisionError> {
            Ok(self.outcome.clone())
        }
    }

    struct BrokenBoolRung;

    #[async_trait]
    impl DecisionFn<bool> for BrokenBoolRung {
        async fn decide(
            &self,
            _input: &str,
        ) -> Result<Option<DecisionOutcome<bool>>, DecisionError> {
            Err(DecisionError::ClassifierUnavailable("offline".to_string()))
        }
    }

    fn bool_outcome(choice: bool, confidence: f32) -> DecisionOutcome<bool> {
        DecisionOutcome {
            rung: DecisionRung::L1,
            choice,
            confidence,
            signal: None,
        }
    }

    #[test]
    fn parse_binary_accepts_bool_yes_no_and_rejects_garbage() {
        assert_eq!(
            parse_binary(r#"{"answer": true, "confidence": 0.9}"#),
            Some((true, 0.9))
        );
        assert_eq!(
            parse_binary(r#"{"answer": "NO", "confidence": 1.5}"#),
            Some((false, 1.0)),
            "string answers and clamped confidence are accepted"
        );
        assert_eq!(
            parse_binary("preamble {\"answer\": false, \"confidence\": 0.7} trailing"),
            Some((false, 0.7))
        );
        assert_eq!(parse_binary("no json"), None);
        assert_eq!(
            parse_binary(r#"{"answer": "maybe", "confidence": 0.9}"#),
            None
        );
        assert_eq!(parse_binary(r#"{"answer": true}"#), None, "score required");
    }

    #[test]
    fn binary_prompt_states_the_anti_echo_rule() {
        let citation = binary_prompt(BinaryDecision::Citation, "CONTENT:\nx\nRESPONSE:\ny");
        assert!(citation.contains("NOT a citation"), "echo must not count");
        assert!(citation.contains("CONTENT"));
        assert!(citation.contains("RESPONSE"));
        assert!(citation.contains("\"answer\""));
        assert!(citation.contains("\"confidence\""));

        let correction = binary_prompt(BinaryDecision::Correction, "that is wrong");
        assert!(correction.contains("NOT a correction"));
        assert!(correction.contains("that is wrong"));
    }

    #[tokio::test]
    async fn binary_gate_without_threshold_never_calls_the_classifier() {
        // T161 stays authoritative: the classifier must not even run.
        let rung = FixedBoolRung {
            outcome: Some(bool_outcome(false, 1.0)),
        };
        let verdict = classify_binary(None, true, &rung, "任何东西").await;
        assert!(verdict, "the heuristic decides when no τ is calibrated");
    }

    #[tokio::test]
    async fn binary_gate_uses_a_confident_classifier_verdict() {
        let rung = FixedBoolRung {
            outcome: Some(bool_outcome(true, 0.95)),
        };
        assert!(classify_binary(Some(0.8), false, &rung, "任何东西").await);

        let rejecting = FixedBoolRung {
            outcome: Some(bool_outcome(false, 0.95)),
        };
        assert!(
            !classify_binary(Some(0.8), true, &rejecting, "任何东西").await,
            "the classifier overrides a heuristic false positive"
        );
    }

    #[tokio::test]
    async fn binary_gate_falls_back_to_the_heuristic_below_threshold_or_on_failure() {
        let timid = FixedBoolRung {
            outcome: Some(bool_outcome(true, 0.3)),
        };
        assert!(
            !classify_binary(Some(0.8), false, &timid, "任何东西").await,
            "below τ the heuristic decides"
        );

        let abstaining = FixedBoolRung { outcome: None };
        assert!(classify_binary(Some(0.8), true, &abstaining, "任何东西").await);

        assert!(
            !classify_binary(Some(0.8), false, &BrokenBoolRung, "任何东西").await,
            "a broken classifier must not rewrite reward bookkeeping"
        );
    }

    #[test]
    fn binary_decision_as_str_roundtrips() {
        assert_eq!(BinaryDecision::Correction.as_str(), "correction");
        assert_eq!(BinaryDecision::Citation.as_str(), "citation");
    }
}
