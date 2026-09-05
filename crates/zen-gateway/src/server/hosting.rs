//! Hosted agent turns (E3) — US4 core: turn state machine, event ring
//! buffer, streaming fan-out, idempotent replay, cancel, and resume.
//!
//! PURPOSE: Owns the E3 [`Turn`] lifecycle
//! (Submitted→Running→Streaming→Completed|Cancelled, AwaitingApproval
//! when an approval is pending) and the per-turn bounded event ring
//! (design §5.3: 2048 frames | 60s) that powers `session/resume`
//! replay. Execution drives [`TurnExecutor::execute_stream`] — the SAME
//! orchestrator streaming API zen-cli uses — so hosted behavior matches
//! the migrated surfaces exactly (user directive: integrate, don't
//! reimplement). [`TurnExecutor`] is a one-method adapter so tests run
//! against scripted executors instead of live providers.
//!
//! USAGE: The daemon installs `session/turn`, `session/cancel`, and
//! `session/resume` handlers capturing a [`SessionHost`]. The turn
//! handler receives the originating connection's outbound queue sender
//! for fan-out (SC-007 anchor); the registry itself is global so resume
//! works across reconnects.
//!
//! EXPECTED: deltas coalesce to ≤1 frame per [`FLUSH_INTERVAL`]
//! (design §5.2); structural events (`tool_*`, `turn_completed`,
//! `turn_error`) are never dropped; tool intermediates stream as
//! `tool_started`/`tool_completed` structural frames (T056, 005
//! `contracts/streaming.md`) parsed from the orchestrator's 🔧/✅
//! callback lines so they survive ring replay; a replayed `turnId`
//! resolves `-32004` with the stored final result instead of
//! re-executing; `session/cancel` drops the execution future
//! mid-await, audits `outcome:"cancelled"`, and answers
//! `{outcome:"cancelled"}`.
//! Turn lifecycle is audited to `audit.jsonl`: `gateway.turn.started`
//! fires once per registration and exactly-once `gateway.turn.completed`
//! (`outcome` completed|cancelled) fires at the FIRST terminal
//! transition (T054 observability).
//!
//! ERRORS: -32602 malformed params; -32003 unknown sessionId;
//! -32004 replayed completed turn; -32603 execution failure /
//! orchestrator unavailable / cancelled turn; -32020 guard rejections
//! surface before the turn is created.

use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, mpsc, watch};
use zen_core::types::{RetrievedNote, SessionContext};

use crate::protocol::RpcError;
use crate::server::connection::OutboundFrame;

/// Per-turn event ring capacity (design §5.3).
pub const RING_CAPACITY: usize = 2048;
/// Ring age window: frames older than this are evicted from the front.
pub const RING_WINDOW: Duration = Duration::from_secs(60);
/// Max latency between token arrival and its delta frame leaving
/// (design §5.2 coalescing tick).
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(33);

// ───────────────────────── executor abstraction ─────────────────────────

/// One-method adapter around the agent stack so hosted execution is
/// drivable by scripted fakes in tests (the daemon binds the real
/// [`zen_agents::AgentOrchestrator`]).
///
/// Owned-token payload (`String`) keeps the signature lifetime-free
/// under `#[async_trait]` — no HRTB annotation, no boxing; call-site
/// closures coerce to `&mut dyn FnMut` automatically.
#[async_trait::async_trait]
pub trait TurnExecutor: Send + Sync {
    async fn execute_stream(
        &self,
        session: &mut SessionContext,
        prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String>;
}

#[async_trait::async_trait]
impl TurnExecutor for zen_agents::AgentOrchestrator {
    async fn execute_stream(
        &self,
        session: &mut SessionContext,
        prompt: &str,
        callback: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<String> {
        self.execute_stream(session, prompt, |tok| callback(tok.to_string()))
            .await
    }
}

// ─────────────────────────── turn entity (E3) ───────────────────────────

/// E3 turn lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// Accepted, execution not started (permit pending).
    Submitted,
    /// Executing; no tokens observed yet.
    Running,
    /// Executing; at least one delta observed.
    Streaming,
    /// Paused on an unanswered approval request.
    AwaitingApproval,
    /// Terminal success; final response retained for replay.
    Completed,
    /// Terminal cancel (client request or watchdog).
    Cancelled,
}

impl TurnState {
    fn is_terminal(self) -> bool {
        matches!(self, TurnState::Completed | TurnState::Cancelled)
    }
}

/// One buffered protocol event with its per-turn monotonic sequence.
#[derive(Debug, Clone)]
pub struct TurnEvent {
    pub seq: u64,
    pub kind: &'static str,
    pub payload: Value,
    #[allow(dead_code)]
    at: Instant,
}

/// E3 record for one hosted turn. Shared between the executing task,
/// the cancel/resume handlers, and the approval broker.
pub struct Turn {
    pub turn_id: String,
    pub session_id: String,
    state: StdMutex<TurnState>,
    events: StdMutex<VecDeque<TurnEvent>>,
    next_seq: AtomicU64,
    response: StdMutex<Option<String>>,
    /// Originating connection's outbound queue (SC-007 routing anchor);
    /// `None` for turns submitted without a live surface (tests).
    origin: StdMutex<Option<mpsc::Sender<OutboundFrame>>>,
    /// Set when a cancellation has been requested; observed via `notify`.
    cancelled: AtomicBool,
    notify_cancel: Notify,
    /// Fires when the turn leaves the running set (any terminal state).
    done_tx: watch::Sender<bool>,
    /// When the turn first entered a terminal state; drives the
    /// oldest-first idempotency reaper so recently-finished turns a
    /// client may still be retrying survive GC.
    terminal_at: StdMutex<Option<Instant>>,
    /// Guards once-only `gateway.turn.completed` emission across
    /// racing terminal paths (client cancel vs watchdog vs natural
    /// completion) — double emission is impossible by construction.
    terminal_audited: AtomicBool,
}

impl Turn {
    fn new(turn_id: String, session_id: String) -> Self {
        let (done_tx, _) = watch::channel(false);
        Self {
            turn_id,
            session_id,
            state: StdMutex::new(TurnState::Submitted),
            events: StdMutex::new(VecDeque::new()),
            next_seq: AtomicU64::new(1),
            response: StdMutex::new(None),
            origin: StdMutex::new(None),
            cancelled: AtomicBool::new(false),
            notify_cancel: Notify::new(),
            done_tx,
            terminal_at: StdMutex::new(None),
            terminal_audited: AtomicBool::new(false),
        }
    }

    pub fn state(&self) -> TurnState {
        *self.state.lock().expect("turn state lock")
    }

    fn set_state(&self, s: TurnState) {
        let mut state = self.state.lock().expect("turn state lock");
        if s.is_terminal() && !state.is_terminal() {
            *self.terminal_at.lock().expect("terminal-at lock") = Some(Instant::now());
        }
        *state = s;
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn final_response(&self) -> Option<String> {
        self.response.lock().expect("response lock").clone()
    }

    /// Snapshot of buffered events with `seq > after` (resume replay).
    pub fn events_after(&self, after: u64) -> Vec<TurnEvent> {
        self.events
            .lock()
            .expect("events lock")
            .iter()
            .filter(|e| e.seq > after)
            .cloned()
            .collect()
    }

    pub fn oldest_seq(&self) -> Option<u64> {
        self.events
            .lock()
            .expect("events lock")
            .front()
            .map(|e| e.seq)
    }

    /// Requests cancellation; idempotent.
    pub fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify_cancel.notify_waiters();
    }

    /// Claims the terminal-audit slot: `true` exactly once per turn.
    fn claim_terminal_audit(&self) -> bool {
        !self.terminal_audited.swap(true, Ordering::SeqCst)
    }

    /// Appends an event to the ring (evicting capacity/window overflow)
    /// and returns its assigned seq.
    fn push_event(&self, kind: &'static str, payload: Value) -> u64 {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let mut ring = self.events.lock().expect("events lock");
        ring.push_back(TurnEvent {
            seq,
            kind,
            payload,
            at: Instant::now(),
        });
        while ring.len() > RING_CAPACITY {
            ring.pop_front();
        }
        let cutoff = Instant::now() - RING_WINDOW;
        while ring.front().is_some_and(|e| e.at < cutoff) {
            ring.pop_front();
        }
        seq
    }

    /// Emits a never-dropped structural event to the ring and the
    /// originating connection.
    fn emit_structural(&self, kind: &'static str, payload: Value) {
        let seq = self.push_event(kind, payload.clone());
        self.send_frame(
            OutboundFrame::structural(crate::protocol::Frame::notification(
                "session/event",
                event_params(&self.turn_id, seq, kind, &payload),
            )),
            kind,
        );
    }

    /// Emits a coalescing-class delta frame (droppable under pressure —
    /// the merged superset always arrives in `turn_completed`).
    fn emit_delta(&self, text: &str) {
        let seq = self.push_event("delta", json!({ "text": text }));
        self.send_frame(
            OutboundFrame::delta(crate::protocol::Frame::notification(
                "session/event",
                event_params(&self.turn_id, seq, "delta", &json!({ "text": text })),
            )),
            "delta",
        );
    }

    /// Emits a tool-lifecycle structural frame (T056, 005
    /// `contracts/streaming.md`).
    ///
    /// Functionality: `tool_started`/`tool_completed` intermediates ride
    /// the same never-dropped path as `turn_completed` — ring buffer
    /// (2048 frames / 60s, replayable via `session/resume`) plus
    /// `OutboundFrame::structural` delivery to the originating surface.
    /// User impact: HTTP/WS clients observe every tool round even under
    /// outbound backpressure, and resume replays them in seq order.
    /// Default: `tool` is merged into `payload`; the caller decides
    /// which optional fields (`args`/`duration_ms`/`count`/`provider`/
    /// `error`/`preview`) are present, matching the contract's
    /// omit-when-absent shape.
    fn emit_tool_event(&self, kind: &'static str, tool: &str, mut payload: Value) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("tool".to_string(), Value::String(tool.to_string()));
        }
        self.emit_structural(kind, payload);
    }

    fn send_frame(&self, frame: OutboundFrame, kind: &'static str) {
        let queue = self.origin.lock().expect("origin lock").clone();
        if let Some(queue) = queue {
            let turn = self.turn_id.clone();
            tokio::spawn(async move {
                if let Err(e) = queue.send(frame).await {
                    tracing::debug!(%turn, kind, "event send failed (connection gone): {e}");
                }
            });
        }
    }
}

fn event_params(turn_id: &str, seq: u64, kind: &str, payload: &Value) -> Value {
    let mut params = json!({ "turnId": turn_id, "seq": seq, "kind": kind });
    let obj = params.as_object_mut().expect("fresh object");
    for (k, v) in payload.as_object().into_iter().flatten() {
        obj.insert(k.clone(), v.clone());
    }
    params
}

// ───────────────────── tool intermediates (T056, D17) ─────────────────────

/// Callback marker opening a tool-start intermediate line.
pub const TOOL_STARTED_MARKER: &str = "🔧";
/// Callback marker opening a tool-completion intermediate line.
pub const TOOL_COMPLETED_MARKER: &str = "✅";
/// Preview cap for `tool_completed` payloads (contracts/streaming.md).
pub const TOOL_PREVIEW_MAX_CHARS: usize = 100;

/// One parsed tool intermediate (research D17 grammar, 005
/// `contracts/streaming.md` "Tool intermediate shapes").
#[derive(Debug, Clone, PartialEq)]
pub enum ToolIntermediate {
    /// `🔧 <tool>[ args…]` — dispatch is starting.
    Started {
        tool: String,
        /// Best-effort: JSON object when the orchestrator emits one,
        /// `{"raw": "<free text>"}` otherwise.
        args: Value,
    },
    /// `✅ <tool> done [N hits] [M ms] [provider=P]` — dispatch
    /// finished (or failed, via `✅ <tool> failed[/error…]: <message>`).
    Completed {
        tool: String,
        count: Option<u64>,
        duration_ms: Option<u64>,
        provider: Option<String>,
        error: Option<String>,
        /// ≤ [`TOOL_PREVIEW_MAX_CHARS`] chars of the result preview.
        preview: Option<String>,
    },
}

/// True when a callback line opens with a tool-intermediate marker.
pub fn is_tool_intermediate(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with(TOOL_STARTED_MARKER) || trimmed.starts_with(TOOL_COMPLETED_MARKER)
}

/// Splits one orchestrator callback token into plain text (feeds the
/// delta pipeline unchanged) and tool intermediates (T056 structural
/// frames).
///
/// PURPOSE: The orchestrator interleaves 🔧/✅ lifecycle lines with
/// streamed LLM text on the single `TurnExecutor` callback (research
/// D17 — no trait break). This is the shared classifier for both
/// consumers: the gateway (structural frames; tool lines suppressed
/// from deltas) and the TUI `StreamCollector` (collapsible blocks).
///
/// GRAMMAR (per line):
///   `🔧 <tool>[ args…]`                          → [`ToolIntermediate::Started`]
///   `✅ <tool> done [N hits] [M ms] [provider=P]` → Completed
///   `✅ <tool> failed[/error…]: <message>`        → Completed { error }
/// The first non-marker line after a `✅` header is its preview
/// (single line, capped at [`TOOL_PREVIEW_MAX_CHARS`] chars per the
/// contract's `"preview": "…100 chars…"` shape); every other line
/// passes through as text. Unrecognized metrics degrade to `tool` +
/// `preview` only.
pub fn split_tool_intermediates(token: &str) -> (String, Vec<ToolIntermediate>) {
    let mut text = String::new();
    let mut events: Vec<ToolIntermediate> = Vec::new();
    let mut collecting_preview = false;
    for line in token.split('\n') {
        if is_tool_intermediate(line) {
            collecting_preview = line.trim_start().starts_with(TOOL_COMPLETED_MARKER);
            if let Some(event) = parse_tool_header(line.trim_start()) {
                events.push(event);
            }
            continue;
        }
        if collecting_preview && !line.trim().is_empty() && !events.is_empty() {
            let wants_preview = matches!(
                events.last(),
                Some(ToolIntermediate::Completed { preview: None, .. })
            );
            if wants_preview {
                append_preview(events.last_mut().expect("checked non-empty"), line);
                continue;
            }
        }
        collecting_preview = false;
        if line.is_empty() && text.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
    (text, events)
}

fn parse_tool_header(line: &str) -> Option<ToolIntermediate> {
    let completed = line.starts_with(TOOL_COMPLETED_MARKER);
    let rest = line
        .strip_prefix(TOOL_COMPLETED_MARKER)
        .or_else(|| line.strip_prefix(TOOL_STARTED_MARKER))?
        .trim();
    let (tool, detail) = match rest.split_once(char::is_whitespace) {
        Some((tool, detail)) => (tool, detail.trim()),
        None => (rest, ""),
    };
    if tool.is_empty() {
        return None;
    }
    if !completed {
        let args = match serde_json::from_str::<Value>(detail) {
            Ok(args @ Value::Object(_)) => args,
            _ if detail.is_empty() => json!({}),
            _ => json!({ "raw": detail }),
        };
        return Some(ToolIntermediate::Started {
            tool: tool.to_string(),
            args,
        });
    }
    let (count, duration_ms, provider) = scan_metrics(detail);
    Some(ToolIntermediate::Completed {
        tool: tool.to_string(),
        count,
        duration_ms,
        provider,
        error: extract_error(detail),
        preview: None,
    })
}

/// Extracts `N hits`, `Mms`, and `provider=P` metrics; absent fields
/// stay `None` (contract omit-when-absent shape).
fn scan_metrics(detail: &str) -> (Option<u64>, Option<u64>, Option<String>) {
    let mut count = None;
    let mut duration_ms = None;
    let mut provider = None;
    let tokens: Vec<&str> = detail.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        if let Some(p) = tok.strip_prefix("provider=") {
            provider = Some(p.trim_matches('"').to_string());
        } else if let Some(n) = tok.strip_suffix("ms").and_then(|n| n.parse::<u64>().ok()) {
            if duration_ms.is_none() {
                duration_ms = Some(n);
            }
        } else if let Ok(n) = tok.parse::<u64>()
            && count.is_none()
            && tokens
                .get(i + 1)
                .is_some_and(|next| next.starts_with("hit"))
        {
            count = Some(n);
        }
    }
    (count, duration_ms, provider)
}

/// Recognizes `failed[: ] <message>` / `error[:=] <message>` in the
/// header detail; ASCII keywords are located case-insensitively with a
/// char-boundary guard so multibyte content cannot slice mid-codepoint.
fn extract_error(detail: &str) -> Option<String> {
    for keyword in ["failed", "error"] {
        if let Some(at) = find_ignore_case(detail, keyword) {
            let rest = detail[at + keyword.len()..]
                .trim_start()
                .trim_start_matches([':', '='])
                .trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn find_ignore_case(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len())
        .find(|&i| hay.is_char_boundary(i) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

fn append_preview(event: &mut ToolIntermediate, line: &str) {
    let ToolIntermediate::Completed { preview, .. } = event else {
        return;
    };
    if preview.is_some() {
        return;
    }
    *preview = Some(truncate_chars(line, TOOL_PREVIEW_MAX_CHARS));
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

// ──────────────────────────── registry + deps ───────────────────────────

/// Global turn registry: idempotency map + per-session execution permits
/// (one running turn per session keeps `SessionContext` mutation safe).
#[derive(Default)]
pub struct TurnRegistry {
    turns: StdMutex<HashMap<String, Arc<Turn>>>,
    permits: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl TurnRegistry {
    /// Inserts `record` atomically, returning the winner when a
    /// concurrent duplicate with the same turnId registered first —
    /// plain `get`-then-`insert` would let two racing submissions both
    /// execute (HashMap::insert overwrites the loser's record).
    fn insert_if_absent(&self, record: Arc<Turn>) -> Option<Arc<Turn>> {
        let mut turns = self.turns.lock().expect("registry lock");
        if let Some(existing) = turns.get(&record.turn_id) {
            return Some(Arc::clone(existing));
        }
        turns.insert(record.turn_id.clone(), record);
        None
    }

    /// Looks up a turn record by id (`None` when GC'd or unknown).
    pub fn get(&self, turn_id: &str) -> Option<Arc<Turn>> {
        self.turns
            .lock()
            .expect("registry lock")
            .get(turn_id)
            .cloned()
    }

    fn remove(&self, turn_id: &str) {
        self.turns.lock().expect("registry lock").remove(turn_id);
    }

    /// Number of turns currently in a non-terminal state.
    pub fn active_count(&self) -> usize {
        self.turns
            .lock()
            .expect("registry lock")
            .values()
            .filter(|t| !t.state().is_terminal())
            .count()
    }

    /// Removes terminal turns beyond `retain`, oldest-terminal-first;
    /// called periodically so the idempotency map stays bounded without
    /// dropping recently-finished turns a client may still retry.
    pub fn reap_terminal(&self, retain: usize) {
        let mut turns = self.turns.lock().expect("registry lock");
        let mut terminal: Vec<(String, Option<Instant>)> = turns
            .iter()
            .filter(|(_, t)| t.state().is_terminal())
            .map(|(id, t)| (id.clone(), *t.terminal_at.lock().expect("terminal-at lock")))
            .collect();
        terminal.sort_by_key(|&(_, at)| at);
        let excess = terminal.len().saturating_sub(retain);
        for (id, _) in terminal.into_iter().take(excess) {
            turns.remove(&id);
        }
    }

    /// Requests cancellation on every non-terminal turn (drain path);
    /// each turn's finalize writes its own cancelled audit.
    pub async fn cancel_all_active(&self) {
        let active: Vec<Arc<Turn>> = self
            .turns
            .lock()
            .expect("registry lock")
            .values()
            .filter(|t| !t.state().is_terminal())
            .cloned()
            .collect();
        for record in active {
            record.request_cancel();
        }
    }

    async fn permit(&self, session_id: &str) -> Arc<Mutex<()>> {
        self.permits
            .lock()
            .expect("permit lock")
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

/// Dependency bundle for the hosted-session methods.
#[derive(Clone)]
pub struct SessionHost {
    /// Daemon-built executor; `None` degrades turns to fast -32603.
    pub executor: Option<Arc<dyn TurnExecutor>>,
    /// Live sessions keyed by sessionId (shared with session/start).
    pub sessions: Arc<Mutex<HashMap<String, SessionContext>>>,
    /// Global turn registry (idempotency + resume source of truth).
    pub turns: Arc<TurnRegistry>,
    /// Guard aggregate (watchdog/breaker/doom-loop).
    pub guards: Arc<crate::server::guards::Guards>,
    /// Approval router bridging sandbox callbacks to Q3 round-trips.
    pub approval: Arc<crate::server::approval::ApprovalBroker>,
    /// Audit JSONL sink for cancellations and guard outcomes; `None`
    /// disables file audit (tests).
    pub audit_path: Option<std::path::PathBuf>,
}

impl SessionHost {
    /// Creates an empty bundle around an optional executor.
    pub fn new(executor: Option<Arc<dyn TurnExecutor>>) -> Self {
        Self {
            executor,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            turns: Arc::new(TurnRegistry::default()),
            guards: Arc::default(),
            approval: Arc::default(),
            audit_path: None,
        }
    }

    /// Appends one structured audit line (`audit.jsonl`). Fire-and-forget:
    /// audit failures log a warning and never fail the operation.
    pub fn audit(&self, record: Value) {
        let Some(path) = self.audit_path.clone() else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let line = format!("{record}\n");
            let write = || -> std::io::Result<()> {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                f.write_all(line.as_bytes())
            };
            if let Err(e) = write() {
                tracing::warn!(path = %path.display(), "gateway audit write failed: {e}");
            }
        });
    }
}

// ─────────────────────────── hosted methods ─────────────────────────────

/// Parses a contract `sensitivity` string case-insensitively. The wire
/// convention is lowercase (`"public"`), while [`Sensitivity`]'s serde
/// form is PascalCase — both are accepted so either surface can send.
pub(crate) fn parse_sensitivity(raw: &str) -> Option<zen_core::types::Sensitivity> {
    match raw.to_lowercase().as_str() {
        "public" => Some(zen_core::types::Sensitivity::Public),
        "private" => Some(zen_core::types::Sensitivity::Private),
        "confidential" => Some(zen_core::types::Sensitivity::Confidential),
        _ => None,
    }
}

/// Extracts required string params (-32602 otherwise).
fn require_str<'a>(
    method: &'static str,
    params: &'a Value,
    key: &str,
) -> Result<&'a str, RpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params(method, &format!("missing \"{key}\"")))
}

/// Parses the optional contract `knowledge` array into
/// [`RetrievedNote`]s (contracts/02 §Session). `Ok(None)` when absent;
/// `-32602` when present but malformed.
pub(crate) fn parse_knowledge(params: &Value) -> Result<Option<Vec<RetrievedNote>>, RpcError> {
    const METHOD: &str = "session/turn";
    let Some(items) = params.get("knowledge").and_then(Value::as_array) else {
        return Ok(None);
    };
    let mut notes = Vec::with_capacity(items.len());
    for item in items {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| RpcError::invalid_params(METHOD, "knowledge item missing \"path\""))?;
        let content = item
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                RpcError::invalid_params(METHOD, "knowledge item missing \"content\"")
            })?;
        let sensitivity = match item.get("sensitivity").and_then(Value::as_str) {
            None => zen_core::types::Sensitivity::Public,
            Some(raw) => parse_sensitivity(raw).ok_or_else(|| {
                RpcError::invalid_params(
                    METHOD,
                    &format!("unknown sensitivity {raw:?}; expected public|private|confidential"),
                )
            })?,
        };
        let relevance = item.get("relevance").and_then(Value::as_f64).unwrap_or(1.0);
        notes.push(RetrievedNote {
            path,
            content,
            sensitivity,
            relevance,
        });
    }
    Ok(Some(notes))
}

/// session/start — `{sessionId?, agent?}` → `{sessionId, agent}`.
/// Mints UUIDv7 ids and registers the hosted [`SessionContext`].
pub async fn start(deps: Arc<SessionHost>, params: Value) -> Result<Value, RpcError> {
    let requested = params.get("sessionId").and_then(Value::as_str);
    let agent = params.get("agent").and_then(Value::as_str);

    let mut sessions = deps.sessions.lock().await;
    let session_id = match requested {
        Some(id) => id.to_string(),
        None => uuid::Uuid::now_v7().to_string(),
    };
    let session = sessions
        .entry(session_id.clone())
        .or_insert_with(|| SessionContext::new(session_id.clone(), String::new()));
    if let Some(agent) = agent {
        session.agent_name = agent.to_string();
    }
    let agent_label = if session.agent_name.is_empty() {
        "auto".to_string()
    } else {
        session.agent_name.clone()
    };
    Ok(json!({ "sessionId": session_id, "agent": agent_label }))
}

/// session/cancel — `{turnId}` → `{outcome:"cancelled"}`.
///
/// Cancelling a finished/unknown turn still reports success (cancel is
/// advisory); only malformed params fail.
pub async fn cancel(deps: Arc<SessionHost>, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "session/cancel";
    let turn_id = require_str(METHOD, &params, "turnId")?.to_string();
    if let Some(record) = deps.turns.get(&turn_id)
        && !record.state().is_terminal()
    {
        record.request_cancel();
    }
    Ok(json!({ "outcome": "cancelled" }))
}

/// session/resume — `{turnId, lastSeq}` → buffered frames after
/// `lastSeq`, or a full snapshot when the window was exceeded (design
/// §5.3). Unknown turns yield an empty snapshot so stale clients recover.
pub async fn resume(deps: Arc<SessionHost>, params: Value) -> Result<Value, RpcError> {
    const METHOD: &str = "session/resume";
    let turn_id = require_str(METHOD, &params, "turnId")?.to_string();
    let last_seq = params.get("lastSeq").and_then(Value::as_u64).unwrap_or(0);

    let Some(record) = deps.turns.get(&turn_id) else {
        return Ok(json!({ "snapshot": true, "response": Value::Null, "events": [] }));
    };

    // Beyond-window detection: a gap between lastSeq and the oldest
    // buffered frame means frames were evicted — replay would tear.
    let gap = record
        .oldest_seq()
        .is_some_and(|oldest| last_seq + 1 < oldest);

    if gap || (record.state().is_terminal() && record.oldest_seq().is_none()) {
        return Ok(json!({
            "snapshot": true,
            "response": record.final_response(),
            "events": [],
        }));
    }

    let events: Vec<Value> = record
        .events_after(last_seq)
        .into_iter()
        .map(|e| event_params(&turn_id, e.seq, e.kind, &e.payload))
        .collect();
    Ok(json!({ "events": events }))
}

/// session/turn — hosted streaming execution. The Q2 response arrives
/// after completion (contracts/02); the dispatcher runs this handler off
/// the recv loop so `session/cancel` stays responsive.
///
/// `origin` routes every emitted frame to the submitting surface only;
/// `connection` (when present) enables Q3 approval routing through the
/// broker for the duration of the turn.
pub async fn turn(
    deps: Arc<SessionHost>,
    origin: Option<mpsc::Sender<OutboundFrame>>,
    params: Value,
) -> Result<Value, RpcError> {
    turn_with(deps, origin, None, params).await
}

/// Full-featured entry point used by the daemon (passes the origin's
/// [`ConnectionHandle`] so gated tool invocations can solicit Q3
/// approvals from this surface).
#[tracing::instrument(skip_all, fields(turn_id, session_id))]
pub async fn turn_with(
    deps: Arc<SessionHost>,
    origin: Option<mpsc::Sender<OutboundFrame>>,
    connection: Option<crate::server::dispatch::ConnectionHandle>,
    params: Value,
) -> Result<Value, RpcError> {
    tracing::Span::current().record("turn_id", params["turnId"].as_str().unwrap_or("?"));
    tracing::Span::current().record("session_id", params["sessionId"].as_str().unwrap_or("?"));
    const METHOD: &str = "session/turn";
    let turn_id = require_str(METHOD, &params, "turnId")?.to_string();
    let session_id = require_str(METHOD, &params, "sessionId")?.to_string();
    let prompt = require_str(METHOD, &params, "prompt")?.to_string();

    let knowledge = parse_knowledge(&params)?;

    // Idempotency peek first: replays and retries of an already-known
    // turnId never consume doom-loop budget or trip the breaker — only
    // genuinely new (or re-run-after-cancel) submissions are guarded.
    if let Some(existing) = deps.turns.get(&turn_id) {
        if !existing.state().is_terminal() {
            // In-flight duplicate: wait for the original to finish, then
            // mirror its terminal outcome.
            let mut done = existing.done_tx.subscribe();
            while !*done.borrow_and_update() {
                if done.changed().await.is_err() {
                    break;
                }
            }
        }
        return match existing.final_response() {
            Some(response) => Err(RpcError::turn_already_completed(json!(response))),
            None => {
                // Cancelled prior attempt: allow a fresh run under the
                // same id (no completed side effects to protect).
                deps.turns.remove(&turn_id);
                Box::pin(turn(deps, origin, params)).await
            }
        };
    }

    // Guard ring before any side effect (design §6 — every rejection
    // auditable).
    if let Err(rejection) = deps.guards.check_submit(&session_id) {
        crate::server::guards::audit_rejection(
            deps.audit_path.as_ref(),
            rejection
                .data
                .as_ref()
                .and_then(|d| d.get("guard"))
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            &rejection.message,
        );
        return Err(rejection);
    }

    // Agent label for the registration audit: the session map is the
    // authority (session/start populated it); empty names fall back
    // to the "auto" convention shared with session/start and
    // agent/status frames.
    let agent_label = {
        let mut sessions = deps.sessions.lock().await;
        if !sessions.contains_key(&session_id) {
            return Err(RpcError::session_not_found(&session_id));
        }
        if let Some(notes) = knowledge {
            sessions
                .get_mut(&session_id)
                .unwrap()
                .knowledge
                .extend(notes);
        }
        sessions
            .get(&session_id)
            .map(|s| s.agent_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "auto".to_string())
    };

    let Some(executor) = deps.executor.as_ref() else {
        return Err(RpcError::internal(
            "agent stack unavailable (config/router init failed)",
        ));
    };

    let record = Arc::new(Turn::new(turn_id.clone(), session_id.clone()));
    *record.origin.lock().expect("origin lock") = origin;
    if let Some(existing) = deps.turns.insert_if_absent(Arc::clone(&record)) {
        // Registration race lost: mirror the winner (no double-exec).
        if !existing.state().is_terminal() {
            let mut done = existing.done_tx.subscribe();
            while !*done.borrow_and_update() {
                if done.changed().await.is_err() {
                    break;
                }
            }
        }
        return match existing.final_response() {
            Some(response) => Err(RpcError::turn_already_completed(json!(response))),
            None => Err(RpcError::internal(
                "turn superseded by concurrent duplicate",
            )),
        };
    }

    // Fresh registration won: replays, retries, and race losers
    // return above without re-registering, so this fires exactly once
    // per registered execution (T054).
    deps.audit(json!({
        "ts": chrono_now(),
        "kind": "gateway.turn.started",
        "turnId": record.turn_id,
        "sessionId": record.session_id,
        "agent": agent_label,
    }));

    // Q3 approval route lives exactly as long as the turn; a deadline
    // miss (-32011) inside the broker cancels it (contracts/02).
    let _approval_guard = connection.map(|handle| {
        let cancel_target = Arc::clone(&record);
        deps.approval.register(turn_id.clone(), handle, move || {
            cancel_target.request_cancel()
        })
    });

    let permit = deps.turns.permit(&session_id).await;
    let _guard = permit.lock().await;

    // Re-check cancellation that raced ahead of permit acquisition.
    if record.is_cancelled() {
        finalize_cancelled(&deps, &record).await;
        return Err(RpcError::internal("turn cancelled"));
    }

    // Snapshot the session context, stream against an owned copy, and
    // merge back — the shared map lock is never held across streaming,
    // so turns on different sessions run truly concurrently.
    let mut ctx = {
        let mut sessions = deps.sessions.lock().await;
        sessions.get_mut(&session_id).expect("checked").clone()
    };

    record.set_state(TurnState::Running);
    emit_agent_status(&record, "busy");

    // Coalescing flusher: drains the pending-token buffer at most once
    // per FLUSH_INTERVAL; exits after the turn's final drain.
    let pending: Arc<StdMutex<String>> = Arc::default();
    let flush_notify = Arc::new(Notify::new());
    let flusher = spawn_flusher(
        Arc::clone(&record),
        Arc::clone(&pending),
        flush_notify.clone(),
    );

    let token_record = Arc::clone(&record);
    let token_pending = Arc::clone(&pending);
    let token_notify = flush_notify.clone();
    let mut callback = move |tok: String| {
        // T056 (research D17): the orchestrator interleaves 🔧/✅ tool
        // lifecycle lines with LLM text on this single callback. Tool
        // lines bypass the coalescing delta buffer and go out as
        // structural frames — dropping a `tool_completed` would tear
        // the event stream like a dropped `turn_completed`, and
        // replaying them as deltas would double-render on clients
        // that understand the structured kinds.
        let (text, tool_events) = split_tool_intermediates(&tok);
        for event in &tool_events {
            match event {
                ToolIntermediate::Started { tool, args } => {
                    token_record.emit_tool_event("tool_started", tool, json!({ "args": args }));
                }
                ToolIntermediate::Completed {
                    tool,
                    count,
                    duration_ms,
                    provider,
                    error,
                    preview,
                } => {
                    let mut payload = serde_json::Map::new();
                    if let Some(c) = count {
                        payload.insert("count".to_string(), json!(c));
                    }
                    if let Some(ms) = duration_ms {
                        payload.insert("duration_ms".to_string(), json!(ms));
                    }
                    if let Some(p) = provider {
                        payload.insert("provider".to_string(), json!(p));
                    }
                    if let Some(e) = error {
                        payload.insert("error".to_string(), json!(e));
                    }
                    if let Some(pv) = preview {
                        payload.insert("preview".to_string(), json!(pv));
                    }
                    token_record.emit_tool_event("tool_completed", tool, Value::Object(payload));
                }
            }
        }
        if text.is_empty() {
            return;
        }
        if token_record.state() == TurnState::Running {
            token_record.set_state(TurnState::Streaming);
        }
        token_pending.lock().expect("pending lock").push_str(&text);
        token_notify.notify_one();
    };

    let watchdog = crate::server::guards::watchdog_timeout();
    let turn_scope = record.turn_id.clone();
    let execution = async move {
        let outcome = zen_agents::APPROVAL_TURN
            .scope(
                Some(turn_scope),
                executor.execute_stream(&mut ctx, &prompt, &mut callback),
            )
            .await;
        (outcome, ctx)
    };

    let cancel_record = Arc::clone(&record);
    let cancelled = async move {
        loop {
            cancel_record.notify_cancel.notified().await;
            if cancel_record.is_cancelled() {
                break;
            }
        }
    };

    let raced = async {
        tokio::select! {
            res = execution => Ok(res),
            () = cancelled => Err(CancelReason::Client),
        }
    };
    let outcome = tokio::time::timeout(watchdog, raced).await;

    // Final delta drain before any terminal event so ordering holds.
    drain_pending(&record, &pending);
    flush_done_signal(&flusher);

    let mut merge_ctx: Option<SessionContext> = None;
    let result = match outcome {
        Ok(Ok((Ok(response), ctx))) => {
            merge_ctx = Some(ctx);
            deps.guards.record_success(&session_id);
            record.set_state(TurnState::Completed);
            *record.response.lock().expect("response lock") = Some(response.clone());
            record.emit_structural("turn_completed", json!({ "response": response }));
            emit_agent_status(&record, "idle");
            release_done(&record);
            audit_turn_completed(&deps, &record, "completed");
            Ok(
                json!({ "turnId": turn_id, "response": record.final_response().unwrap_or_default() }),
            )
        }
        Ok(Ok((Err(e), ctx))) => {
            merge_ctx = Some(ctx);
            deps.guards.record_failure(&session_id);
            record.set_state(TurnState::Completed);
            let message = format!("agent execution failed: {e:#}");
            record.emit_structural("turn_error", json!({ "code": -32603, "message": message }));
            emit_agent_status(&record, "error");
            release_done(&record);
            audit_turn_completed(&deps, &record, "completed");
            Err(RpcError::internal(&format!("agent execution failed: {e}")))
        }
        Ok(Err(reason)) => {
            finalize_cancelled_with_reason(&deps, &record, reason).await;
            Err(RpcError::internal("turn cancelled"))
        }
        Err(_elapsed) => {
            // Watchdog fired: contains hung LLM/tool calls (design §6).
            deps.guards.record_failure(&session_id);
            record.emit_structural(
                "turn_error",
                json!({
                    "code": -32020,
                    "message": "watchdog timeout",
                    "guard": "watchdog",
                }),
            );
            finalize_cancelled_with_reason(&deps, &record, CancelReason::Watchdog).await;
            Err(crate::server::guards::watchdog_rejected())
        }
    };

    if let Some(ctx) = merge_ctx {
        let mut sessions = deps.sessions.lock().await;
        if let Some(slot) = sessions.get_mut(&session_id) {
            *slot = ctx;
        }
    }
    result
}

enum CancelReason {
    Client,
    Watchdog,
}

/// Emits the once-only `gateway.turn.completed` audit line at the
/// turn's FIRST terminal transition; the claim flag makes double
/// emission impossible by construction even when cancel, watchdog,
/// and natural completion race. Outcome follows the existing audit
/// vocabulary ("completed"/"cancelled") — an execution error is still
/// a terminal `Completed` per the TurnState design, its detail lives
/// in the `turn_error` event. `TurnExecutor` reports no token usage,
/// so the `tokens` field is omitted consistently rather than guessed.
fn audit_turn_completed(deps: &Arc<SessionHost>, record: &Arc<Turn>, outcome: &'static str) {
    if !record.claim_terminal_audit() {
        return;
    }
    deps.audit(json!({
        "ts": chrono_now(),
        "kind": "gateway.turn.completed",
        "turnId": record.turn_id,
        "sessionId": record.session_id,
        "outcome": outcome,
    }));
}

async fn finalize_cancelled(deps: &Arc<SessionHost>, record: &Arc<Turn>) {
    finalize_cancelled_with_reason(deps, record, CancelReason::Client).await;
}

async fn finalize_cancelled_with_reason(
    deps: &Arc<SessionHost>,
    record: &Arc<Turn>,
    reason: CancelReason,
) {
    record.set_state(TurnState::Cancelled);
    let (kind, note) = match reason {
        CancelReason::Client => ("cancelled", "client requested"),
        CancelReason::Watchdog => ("watchdog", "watchdog deadline"),
    };
    record.emit_structural(
        "turn_error",
        json!({ "code": -32603, "message": format!("turn {note}") }),
    );
    emit_agent_status(record, "idle");
    release_done(record);
    audit_turn_completed(deps, record, "cancelled");
    deps.audit(json!({
        "ts": chrono_now(),
        "kind": "gateway.cancelled",
        "turnId": record.turn_id,
        "sessionId": record.session_id,
        "reason": kind,
        "outcome": "cancelled",
    }));
}

fn release_done(record: &Arc<Turn>) {
    record.done_tx.send_replace(true);
}

/// Flushes accumulated token text as one merged delta frame.
fn drain_pending(record: &Arc<Turn>, pending: &Arc<StdMutex<String>>) {
    let drained: String = std::mem::take(&mut *pending.lock().expect("pending lock"));
    if !drained.is_empty() {
        record.emit_delta(&drained);
    }
}

fn flush_done_signal(flusher: &tokio::task::JoinHandle<()>) {
    flusher.abort();
}

fn spawn_flusher(
    record: Arc<Turn>,
    pending: Arc<StdMutex<String>>,
    notify: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // first tick fires immediately
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = notify.notified() => {
                    // Small batch window so bursts coalesce instead of
                    // emitting one frame per token.
                    tokio::time::sleep(Duration::from_millis(16)).await;
                }
            }
            drain_pending(&record, &pending);
            if record.state().is_terminal() && pending.lock().expect("pending lock").is_empty() {
                break;
            }
        }
        drain_pending(&record, &pending);
    })
}

fn emit_agent_status(record: &Arc<Turn>, state: &str) {
    record.send_frame(
        OutboundFrame::structural(crate::protocol::Frame::notification(
            "agent/status",
            json!({ "agent": "auto", "state": state, "budgetConsumed": 0 }),
        )),
        "agent/status",
    );
}

/// Best-effort wall-clock timestamp for audit lines.
fn chrono_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::in_process;

    async fn deps_with_executor() -> (Arc<SessionHost>, Arc<TestExec>) {
        let exec = Arc::new(TestExec::default());
        let deps = SessionHost::new(Some(exec.clone() as Arc<dyn TurnExecutor>));
        deps.sessions
            .lock()
            .await
            .insert("s1".into(), SessionContext::new("s1".into(), String::new()));
        (Arc::new(deps), exec)
    }

    /// Scripted executor: streams fragments then returns a canned reply.
    #[derive(Default)]
    struct TestExec {
        delay_ms: StdMutex<u64>,
        runs: std::sync::atomic::AtomicU32,
    }

    impl TestExec {
        fn runs(&self) -> u32 {
            self.runs.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl TurnExecutor for TestExec {
        async fn execute_stream(
            &self,
            _session: &mut SessionContext,
            _prompt: &str,
            callback: &mut (dyn FnMut(String) + Send),
        ) -> anyhow::Result<String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let delay = *self.delay_ms.lock().unwrap();
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            callback("hel".to_string());
            callback("lo ".to_string());
            callback("world".to_string());
            Ok("hello world".to_string())
        }
    }

    fn turn_params(turn: &str, session: &str) -> Value {
        json!({"turnId": turn, "sessionId": session, "prompt": "hi"})
    }

    #[test]
    fn parse_knowledge_absent_yields_none() {
        let params = json!({"turnId": "t", "sessionId": "s", "prompt": "p"});
        assert!(parse_knowledge(&params).unwrap().is_none());
    }

    #[test]
    fn parse_knowledge_maps_contract_items_with_lenient_sensitivity() {
        let params = json!({
            "knowledge": [
                {"path": "inbox/a.md", "content": "alpha", "sensitivity": "public", "relevance": 0.9},
                {"path": "wiki/b.md", "content": "beta", "sensitivity": "Confidential"},
                {"path": "wiki/c.md", "content": "gamma"},
            ]
        });
        let notes = parse_knowledge(&params).unwrap().unwrap();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0].path, "inbox/a.md");
        assert_eq!(notes[0].relevance, 0.9);
        assert_eq!(
            notes[1].sensitivity,
            zen_core::types::Sensitivity::Confidential
        );
        assert_eq!(notes[2].sensitivity, zen_core::types::Sensitivity::Public);
        assert_eq!(notes[2].relevance, 1.0);
    }

    #[test]
    fn parse_knowledge_rejects_missing_path_and_unknown_sensitivity() {
        let missing = json!({"knowledge": [{"content": "x"}]});
        assert_eq!(parse_knowledge(&missing).unwrap_err().code, -32602);

        let bad = json!({"knowledge": [
            {"path": "p", "content": "c", "sensitivity": "secret"}
        ]});
        assert_eq!(parse_knowledge(&bad).unwrap_err().code, -32602);
    }

    #[tokio::test]
    async fn turn_streams_completes_and_returns_response() {
        let (deps, _exec) = deps_with_executor().await;
        let result = turn(Arc::clone(&deps), None, turn_params("t1", "s1"))
            .await
            .unwrap();
        assert_eq!(result["turnId"], "t1");
        assert_eq!(result["response"], "hello world");

        let record = deps.turns.get("t1").unwrap();
        assert_eq!(record.state(), TurnState::Completed);
        assert_eq!(record.final_response().as_deref(), Some("hello world"));
        // Deltas coalesced + turn_completed structural present.
        let kinds: Vec<_> = record.events_after(0).iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&"delta"));
        assert_eq!(*kinds.last().unwrap(), "turn_completed");
    }

    #[tokio::test]
    async fn replayed_turn_id_returns_minus_32004_without_reexecution() {
        let (deps, exec) = deps_with_executor().await;
        turn(Arc::clone(&deps), None, turn_params("t1", "s1"))
            .await
            .unwrap();
        let runs_before = exec.runs();
        let err = turn(Arc::clone(&deps), None, turn_params("t1", "s1"))
            .await
            .unwrap_err();
        assert_eq!((err.code, err.name), (-32004, "turn-already-completed"));
        assert_eq!(err.data.unwrap()["response"], "hello world");
        assert_eq!(exec.runs(), runs_before, "must not re-execute");
    }

    #[tokio::test]
    async fn unknown_session_rejected_with_minus_32003() {
        let (deps, _exec) = deps_with_executor().await;
        let err = turn(Arc::clone(&deps), None, turn_params("t1", "nope"))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    #[tokio::test]
    async fn missing_params_fail_with_minus_32602() {
        let (deps, _exec) = deps_with_executor().await;
        let err = turn(Arc::clone(&deps), None, json!({"turnId": "t"}))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[tokio::test]
    async fn cancel_mid_flight_marks_cancelled_and_audits() {
        let dir = tempfile::tempdir().unwrap();
        let exec = Arc::new(TestExec {
            delay_ms: StdMutex::new(5_000),
            runs: std::sync::atomic::AtomicU32::new(0),
        });
        let mut deps_builder = SessionHost::new(Some(exec as Arc<dyn TurnExecutor>));
        deps_builder
            .sessions
            .lock()
            .await
            .insert("s1".into(), SessionContext::new("s1".into(), String::new()));
        deps_builder.audit_path = Some(dir.path().join("audit.jsonl"));
        let deps = Arc::new(deps_builder);

        let host = tokio::spawn({
            let deps = Arc::clone(&deps);
            async move { turn(deps, None, turn_params("t9", "s1")).await }
        });
        // Give the host a beat to register, then cancel.
        for _ in 0..100 {
            if deps.turns.get("t9").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let cancelled = cancel(Arc::clone(&deps), json!({"turnId": "t9"}))
            .await
            .unwrap();
        assert_eq!(cancelled["outcome"], "cancelled");

        let err = host.await.unwrap().unwrap_err();
        assert!(err.message.contains("cancel"));

        let record = deps.turns.get("t9").unwrap();
        assert_eq!(record.state(), TurnState::Cancelled);
        // Audit lines are written from a blocking task; poll briefly.
        let audit_path = dir.path().join("audit.jsonl");
        let mut audit = String::new();
        for _ in 0..100 {
            if let Ok(content) = std::fs::read_to_string(&audit_path) {
                audit = content;
                if audit.contains("\"outcome\":\"cancelled\"") {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(audit.contains("\"outcome\":\"cancelled\""), "{audit}");
    }

    #[tokio::test]
    async fn cancelled_turn_id_allows_fresh_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let exec = Arc::new(TestExec {
            delay_ms: StdMutex::new(5_000),
            runs: std::sync::atomic::AtomicU32::new(0),
        });
        let mut deps_builder = SessionHost::new(Some(exec.clone() as Arc<dyn TurnExecutor>));
        deps_builder
            .sessions
            .lock()
            .await
            .insert("s1".into(), SessionContext::new("s1".into(), String::new()));
        deps_builder.audit_path = Some(dir.path().join("audit.jsonl"));
        let deps = Arc::new(deps_builder);

        let host = tokio::spawn({
            let deps = Arc::clone(&deps);
            async move { turn(deps, None, turn_params("t10", "s1")).await }
        });
        for _ in 0..100 {
            if deps.turns.get("t10").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        cancel(Arc::clone(&deps), json!({"turnId": "t10"}))
            .await
            .unwrap();
        let _ = host.await.unwrap().unwrap_err();
        assert_eq!(deps.turns.get("t10").unwrap().state(), TurnState::Cancelled);

        *exec.delay_ms.lock().unwrap() = 0;
        let rerun = turn(Arc::clone(&deps), None, turn_params("t10", "s1"))
            .await
            .unwrap();
        assert_eq!(rerun["response"], json!("hello world"));
        assert_eq!(exec.runs(), 2, "cancelled attempt + rerun both executed");
    }

    #[tokio::test]
    async fn resume_replays_frames_after_last_seq() {
        let (deps, _exec) = deps_with_executor().await;
        turn(Arc::clone(&deps), None, turn_params("t1", "s1"))
            .await
            .unwrap();
        let all = resume(Arc::clone(&deps), json!({"turnId": "t1", "lastSeq": 0}))
            .await
            .unwrap();
        assert!(all["events"].as_array().unwrap().len() >= 2);

        let partial = resume(Arc::clone(&deps), json!({"turnId": "t1", "lastSeq": 1}))
            .await
            .unwrap();
        let events = partial["events"].as_array().unwrap();
        assert!(events.iter().all(|e| e["seq"].as_u64().unwrap() > 1));

        let unknown = resume(Arc::clone(&deps), json!({"turnId": "zz", "lastSeq": 5}))
            .await
            .unwrap();
        assert_eq!(unknown["snapshot"], true);
    }

    #[tokio::test]
    async fn concurrent_duplicate_waits_for_original_result() {
        let (deps, _exec) = deps_with_executor().await;
        let d2 = Arc::clone(&deps);
        let first =
            tokio::spawn(async move { turn(d2, None, turn_params("t1", "s1")).await.unwrap() });
        for _ in 0..100 {
            if deps.turns.get("t1").is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let err = turn(Arc::clone(&deps), None, turn_params("t1", "s1"))
            .await
            .unwrap_err();
        assert_eq!(err.code, -32004);
        let _ = first.await.unwrap();
    }

    #[tokio::test]
    async fn ring_evicts_beyond_capacity_keeping_recent() {
        let record = Arc::new(Turn::new("t".into(), "s".into()));
        for i in 0..(RING_CAPACITY + 50) {
            record.push_event("delta", json!({ "text": i }));
        }
        let ring_len = record.events_after(0).len();
        assert_eq!(ring_len, RING_CAPACITY);
        let seqs: Vec<u64> = record.events_after(0).iter().map(|e| e.seq).collect();
        assert_eq!(*seqs.first().unwrap(), 51u64, "oldest evicted");
        assert_eq!(*seqs.last().unwrap(), (RING_CAPACITY + 50) as u64);
    }

    #[tokio::test]
    async fn events_route_to_origin_connection_only() {
        let (_conn_tx, _conn_rx) = in_process::pair();
        let (queue_tx, mut queue_rx) = tokio::sync::mpsc::channel::<OutboundFrame>(64);
        let (deps, _exec) = deps_with_executor().await;
        turn(
            Arc::clone(&deps),
            Some(queue_tx.clone()),
            turn_params("t1", "s1"),
        )
        .await
        .unwrap();

        let mut saw_completed = false;
        while let Some(of) = queue_rx.recv().await {
            if let crate::protocol::Frame::Notification { method, params, .. } = &of.frame {
                if method != "session/event" {
                    continue; // control-plane frames (agent/status) interleave
                }
                if params["kind"] == "turn_completed" {
                    saw_completed = true;
                    break;
                }
            }
        }
        assert!(saw_completed);
    }

    /// Task B2 pinning test: `turn_completed` is a Structural-class frame,
    /// so it must survive a saturated outbound queue (backpressure, not
    /// drop). If lifecycle events get reclassified as deltas this fails
    /// because enqueue drops deltas under pressure.
    #[tokio::test]
    async fn turn_completed_delivery_survives_saturated_outbound_queue() {
        let (deps, _exec) = deps_with_executor().await;
        let (queue_tx, mut queue_rx) = mpsc::channel::<OutboundFrame>(1);
        // Saturate BEFORE the turn runs so every emission hits pressure.
        queue_tx
            .send(OutboundFrame::structural(
                crate::protocol::Frame::notification("prefill", json!({})),
            ))
            .await
            .unwrap();

        turn(
            Arc::clone(&deps),
            Some(queue_tx),
            turn_params("t-sat", "s1"),
        )
        .await
        .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let mut saw_prefill = false;
        let mut saw_completed = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), queue_rx.recv()).await {
                Ok(Some(of)) => {
                    if let crate::protocol::Frame::Notification { method, params, .. } = &of.frame {
                        if method == "prefill" {
                            saw_prefill = true;
                        } else if method == "session/event" && params["kind"] == "turn_completed" {
                            saw_completed = true;
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
        assert!(
            saw_prefill && saw_completed,
            "turn_completed must arrive AFTER the pre-saturated frame (FIFO backpressure proves structural classification)"
        );
    }

    /// Scripted executor emitting 🔧/✅ tool intermediates between token
    /// streams (T056 contract harness, contracts/streaming.md).
    struct ToolExec;

    #[async_trait::async_trait]
    impl TurnExecutor for ToolExec {
        async fn execute_stream(
            &self,
            _session: &mut SessionContext,
            _prompt: &str,
            callback: &mut (dyn FnMut(String) + Send),
        ) -> anyhow::Result<String> {
            callback("🔧 web.search — searching…\n".to_string());
            callback("partial ".to_string());
            callback(
                "✅ web.search done 5 hits 1234ms provider=brave\nfirst result preview text\n"
                    .to_string(),
            );
            callback("final answer".to_string());
            Ok("final answer".to_string())
        }
    }

    async fn deps_with_tool_executor() -> Arc<SessionHost> {
        let deps = SessionHost::new(Some(Arc::new(ToolExec) as Arc<dyn TurnExecutor>));
        deps.sessions
            .lock()
            .await
            .insert("s1".into(), SessionContext::new("s1".into(), String::new()));
        Arc::new(deps)
    }

    /// Contract (005 streaming.md): `tool_started` precedes the first
    /// `delta`, `tool_completed` precedes `turn_completed`, tool lines
    /// never leak into delta payloads, and completion fields survive
    /// into the replayable ring.
    #[tokio::test]
    async fn tool_intermediates_order_and_payloads() {
        let deps = deps_with_tool_executor().await;
        turn(Arc::clone(&deps), None, turn_params("t-tool", "s1"))
            .await
            .unwrap();
        let record = deps.turns.get("t-tool").unwrap();
        let events = record.events_after(0);
        let kinds: Vec<&str> = events.iter().map(|e| e.kind).collect();
        let started = kinds.iter().position(|k| *k == "tool_started").unwrap();
        let first_delta = kinds.iter().position(|k| *k == "delta").unwrap();
        let completed = kinds.iter().position(|k| *k == "tool_completed").unwrap();
        let done = kinds.iter().position(|k| *k == "turn_completed").unwrap();
        assert!(started < first_delta, "kinds: {kinds:?}");
        assert!(completed < done, "kinds: {kinds:?}");

        assert_eq!(events[started].payload["tool"], "web.search");
        assert_eq!(events[started].payload["args"]["raw"], "— searching…");
        assert_eq!(events[completed].payload["tool"], "web.search");
        assert_eq!(events[completed].payload["count"], 5);
        assert_eq!(events[completed].payload["duration_ms"], 1234);
        assert_eq!(events[completed].payload["provider"], "brave");
        assert_eq!(
            events[completed].payload["preview"],
            "first result preview text"
        );

        for event in &events {
            if event.kind == "delta" {
                let text = event.payload["text"].as_str().unwrap_or("");
                assert!(!text.contains('🔧') && !text.contains('✅'), "{text:?}");
            }
        }
    }

    /// Wire check: tool structural frames reach the originating surface
    /// in emission order relative to deltas and the terminal frame.
    #[tokio::test]
    async fn tool_frames_reach_origin_in_emission_order() {
        let deps = deps_with_tool_executor().await;
        let (queue_tx, mut queue_rx) = mpsc::channel::<OutboundFrame>(64);
        turn(
            Arc::clone(&deps),
            Some(queue_tx),
            turn_params("t-wire", "s1"),
        )
        .await
        .unwrap();

        let mut kinds: Vec<String> = Vec::new();
        while let Some(of) = queue_rx.recv().await {
            if let crate::protocol::Frame::Notification { method, params, .. } = &of.frame {
                if method != "session/event" {
                    continue;
                }
                kinds.push(params["kind"].as_str().unwrap_or("?").to_string());
                if params["kind"] == "turn_completed" {
                    break;
                }
            }
        }
        let started = kinds.iter().position(|k| k == "tool_started").unwrap();
        let first_delta = kinds.iter().position(|k| k == "delta").unwrap();
        let completed = kinds.iter().position(|k| k == "tool_completed").unwrap();
        let done = kinds.iter().position(|k| k == "turn_completed").unwrap();
        assert!(
            started < first_delta && completed < done,
            "kinds: {kinds:?}"
        );
    }

    /// Error intermediate: `✅ <tool> failed: …` maps to
    /// `tool_completed {error}` with metrics omitted.
    #[tokio::test]
    async fn tool_error_intermediate_carries_error_field() {
        struct ToolErrExec;
        #[async_trait::async_trait]
        impl TurnExecutor for ToolErrExec {
            async fn execute_stream(
                &self,
                _session: &mut SessionContext,
                _prompt: &str,
                callback: &mut (dyn FnMut(String) + Send),
            ) -> anyhow::Result<String> {
                callback("✅ web.search failed: rate limit 429\n".to_string());
                Ok("no results".to_string())
            }
        }
        let deps = SessionHost::new(Some(Arc::new(ToolErrExec) as Arc<dyn TurnExecutor>));
        deps.sessions
            .lock()
            .await
            .insert("s1".into(), SessionContext::new("s1".into(), String::new()));
        let deps = Arc::new(deps);
        turn(Arc::clone(&deps), None, turn_params("t-err", "s1"))
            .await
            .unwrap();

        let events = deps.turns.get("t-err").unwrap().events_after(0);
        let done = events
            .iter()
            .find(|e| e.kind == "tool_completed")
            .expect("tool_completed in ring");
        assert_eq!(done.payload["tool"], "web.search");
        assert_eq!(done.payload["error"], "rate limit 429");
        assert!(done.payload.get("count").is_none());
        assert!(done.payload.get("duration_ms").is_none());
        assert!(done.payload.get("provider").is_none());
    }

    #[test]
    fn parse_tool_header_started_with_json_args() {
        let parsed =
            split_tool_intermediates("🔧 web.search {\"query\":\"rust\",\"max_results\":5}")
                .1
                .remove(0);
        match parsed {
            ToolIntermediate::Started { tool, args } => {
                assert_eq!(tool, "web.search");
                assert_eq!(args["query"], "rust");
                assert_eq!(args["max_results"], 5);
            }
            other => panic!("expected Started, got {other:?}"),
        }
    }

    #[test]
    fn split_keeps_surrounding_text_and_extracts_preview() {
        let (text, events) = split_tool_intermediates(
            "before\n✅ web.search done 2 hits 90ms provider=ddg\npreview line\nafter",
        );
        assert_eq!(text, "before\nafter");
        assert_eq!(events.len(), 1);
        match &events[0] {
            ToolIntermediate::Completed {
                tool,
                count,
                duration_ms,
                provider,
                error,
                preview,
            } => {
                assert_eq!(tool, "web.search");
                assert_eq!(*count, Some(2));
                assert_eq!(*duration_ms, Some(90));
                assert_eq!(provider.as_deref(), Some("ddg"));
                assert!(error.is_none());
                assert_eq!(preview.as_deref(), Some("preview line"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn preview_is_capped_at_100_chars() {
        let long = "x".repeat(250);
        let (_, events) =
            split_tool_intermediates(&format!("✅ web.search done 1 hits 1ms\n{long}\n"));
        match &events[0] {
            ToolIntermediate::Completed { preview, .. } => {
                let preview = preview.as_deref().unwrap();
                assert_eq!(preview.chars().count(), TOOL_PREVIEW_MAX_CHARS);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }
}
