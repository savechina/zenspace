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
//! None yet: this module is **shadow (observation) mode only**. The L1 rung
//! runs after the production intent decision and records a `loop.decision`
//! audit line; it never influences routing, agent selection, or any
//! production decision. No threshold τ is invented or applied here — that is
//! T169, blocked on labeled calibration data (T173).
//!
//! # Default behavior
//! Shadow observation is off (`[agentic.intent] shadow_embedding = false`).
//! Enabling it starts accumulating calibration data: L1 vs production
//! disagreements are the highest-information samples for T173's harness.
//!
//! # Interaction
//! The embedder is injectable ([`TextEmbedder`]) so tests use a deterministic
//! fake without ONNX; production uses [`VaultTextEmbedder`] over the existing
//! local embedding path (`zen_vault::compute_embeddings_for_text`) —
//! `Sensitivity::Private` locality is preserved, no cloud call at L1.

use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use tracing::warn;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::DefaultRouter;

use crate::intent::{Intent, IntentCategory, IntentSource};

/// Which rung of the decision ladder produced an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionRung {
    /// L0 — deterministic Rust (keyword/INTENT_SIGNALS, guards, budgets).
    L0,
    /// L1 — calibrated decision layer (embedding router; later a local
    /// GBNF-constrained classifier). Shadow-only until thresholds are
    /// calibrated (T169).
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
#[derive(Debug, Clone)]
pub struct DecisionOutcome<C> {
    pub rung: DecisionRung,
    pub choice: C,
    pub confidence: f32,
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
        }))
    }
}

/// Seed intent-example table (T168). A handful of example utterances per
/// [`IntentCategory`], matched by cosine similarity over embeddings. This is
/// a **SEED** — replace with real examples distilled from labeled decision
/// data once T173's labels accumulate. No threshold is applied: the router
/// always returns the argmax match and its cosine similarity as confidence.
pub const SEED_INTENT_EXAMPLES: &[(&str, IntentCategory)] = &[
    // Query — search or read knowledge.
    ("search my notes for tokio", IntentCategory::Query),
    ("what do I know about rust", IntentCategory::Query),
    ("find information about sqlite", IntentCategory::Query),
    ("research the tokio runtime", IntentCategory::Query),
    // Action — create, modify, or execute something.
    ("implement a new function", IntentCategory::Action),
    ("refactor this code", IntentCategory::Action),
    ("write a note about today", IntentCategory::Action),
    ("create a new wiki page", IntentCategory::Action),
    // System — manage configuration or services.
    ("check the gateway status", IntentCategory::System),
    ("restart the daemon", IntentCategory::System),
    ("show my configuration", IntentCategory::System),
    ("manage my providers", IntentCategory::System),
    // Conversation — chat, help, or clarification.
    ("tell me a joke", IntentCategory::Conversation),
    ("what can you do", IntentCategory::Conversation),
    ("help me understand", IntentCategory::Conversation),
    ("just chatting", IntentCategory::Conversation),
];

struct Example {
    category: IntentCategory,
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
        for &(text, category) in SEED_INTENT_EXAMPLES {
            match self.embedder.embed(text).await {
                Ok(embedding) => computed.push(Example {
                    category,
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
            "l1_confidence": outcome.confidence,
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
        IntentSource::Llm => DecisionRung::L2.as_str(),
        IntentSource::Keyword | IntentSource::Fallback => DecisionRung::L0.as_str(),
    }
}

fn default_l1_router() -> &'static EmbeddingIntentRouter<Arc<dyn TextEmbedder>> {
    static ROUTER: OnceLock<EmbeddingIntentRouter<Arc<dyn TextEmbedder>>> = OnceLock::new();
    ROUTER.get_or_init(|| EmbeddingIntentRouter::new(Arc::new(VaultTextEmbedder)))
}

/// Append one `loop.decision` line to `<logs>/audit.jsonl` — same file and
/// style as the `loop.turn.review` line (additive; existing fields untouched).
fn append_decision_audit(paths: &ZenPaths, entry: &serde_json::Value) {
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
}
