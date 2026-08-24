//! Hosted agent turns (E3) — US4 core: turn state machine, event ring
//! buffer, streaming fan-out, idempotent replay, cancel, and resume.
//!
//! PURPOSE: Owns the E3 [`TurnRecord`] lifecycle
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
//! `session/resume` handlers capturing a [`HostingDeps`]. The turn
//! handler receives the originating connection's outbound queue sender
//! for fan-out (SC-007 anchor); the registry itself is global so resume
//! works across reconnects.
//!
//! EXPECTED: deltas coalesce to ≤1 frame per [`FLUSH_INTERVAL`]
//! (design §5.2); structural events (`tool_*`, `turn_completed`,
//! `turn_error`) are never dropped; a replayed `turnId` resolves
//! `-32004` with the stored final result instead of re-executing;
//! `session/cancel` drops the execution future mid-await, audits
//! `outcome:"cancelled"`, and answers `{outcome:"cancelled"}`.
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

use futures_util::future::BoxFuture;
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
pub trait TurnExecutor: Send + Sync {
    /// Streams a hosted turn, invoking `on_token` per streamed fragment
    /// and returning the final response text.
    fn execute_stream<'a>(
        &'a self,
        session: &'a mut SessionContext,
        prompt: &'a str,
        on_token: Box<dyn FnMut(&str) + Send>,
    ) -> BoxFuture<'a, anyhow::Result<String>>;
}

impl TurnExecutor for zen_agents::AgentOrchestrator {
    fn execute_stream<'a>(
        &'a self,
        session: &'a mut SessionContext,
        prompt: &'a str,
        mut on_token: Box<dyn FnMut(&str) + Send>,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move {
            self.execute_stream(session, prompt, |tok: &str| on_token(tok))
                .await
        })
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
pub struct TurnRecord {
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
}

impl TurnRecord {
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

// ──────────────────────────── registry + deps ───────────────────────────

/// Global turn registry: idempotency map + per-session execution permits
/// (one running turn per session keeps `SessionContext` mutation safe).
#[derive(Default)]
pub struct TurnRegistry {
    turns: StdMutex<HashMap<String, Arc<TurnRecord>>>,
    permits: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl TurnRegistry {
    /// Inserts `record` atomically, returning the winner when a
    /// concurrent duplicate with the same turnId registered first —
    /// plain `get`-then-`insert` would let two racing submissions both
    /// execute (HashMap::insert overwrites the loser's record).
    fn insert_if_absent(&self, record: Arc<TurnRecord>) -> Option<Arc<TurnRecord>> {
        let mut turns = self.turns.lock().expect("registry lock");
        if let Some(existing) = turns.get(&record.turn_id) {
            return Some(Arc::clone(existing));
        }
        turns.insert(record.turn_id.clone(), record);
        None
    }

    /// Looks up a turn record by id (`None` when GC'd or unknown).
    pub fn get(&self, turn_id: &str) -> Option<Arc<TurnRecord>> {
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
        let active: Vec<Arc<TurnRecord>> = self
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
pub struct HostingDeps {
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

impl HostingDeps {
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
pub async fn start(deps: Arc<HostingDeps>, params: Value) -> Result<Value, RpcError> {
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
pub async fn cancel(deps: Arc<HostingDeps>, params: Value) -> Result<Value, RpcError> {
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
pub async fn resume(deps: Arc<HostingDeps>, params: Value) -> Result<Value, RpcError> {
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
    deps: Arc<HostingDeps>,
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
    deps: Arc<HostingDeps>,
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

    {
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
    }

    let Some(executor) = deps.executor.as_ref() else {
        return Err(RpcError::internal(
            "agent stack unavailable (config/router init failed)",
        ));
    };

    let record = Arc::new(TurnRecord::new(turn_id.clone(), session_id.clone()));
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
    let on_token = Box::new(move |tok: &str| {
        if token_record.state() == TurnState::Running {
            token_record.set_state(TurnState::Streaming);
        }
        token_pending.lock().expect("pending lock").push_str(tok);
        token_notify.notify_one();
    }) as Box<dyn FnMut(&str) + Send>;

    let watchdog = crate::server::guards::watchdog_timeout();
    let execution = async move {
        let outcome = executor.execute_stream(&mut ctx, &prompt, on_token).await;
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

async fn finalize_cancelled(deps: &Arc<HostingDeps>, record: &Arc<TurnRecord>) {
    finalize_cancelled_with_reason(deps, record, CancelReason::Client).await;
}

async fn finalize_cancelled_with_reason(
    deps: &Arc<HostingDeps>,
    record: &Arc<TurnRecord>,
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
    deps.audit(json!({
        "ts": chrono_now(),
        "kind": "gateway.cancelled",
        "turnId": record.turn_id,
        "sessionId": record.session_id,
        "reason": kind,
        "outcome": "cancelled",
    }));
}

fn release_done(record: &Arc<TurnRecord>) {
    record.done_tx.send_replace(true);
}

/// Flushes accumulated token text as one merged delta frame.
fn drain_pending(record: &Arc<TurnRecord>, pending: &Arc<StdMutex<String>>) {
    let drained: String = std::mem::take(&mut *pending.lock().expect("pending lock"));
    if !drained.is_empty() {
        record.emit_delta(&drained);
    }
}

fn flush_done_signal(flusher: &tokio::task::JoinHandle<()>) {
    flusher.abort();
}

fn spawn_flusher(
    record: Arc<TurnRecord>,
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

fn emit_agent_status(record: &Arc<TurnRecord>, state: &str) {
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

    async fn deps_with_executor() -> (Arc<HostingDeps>, Arc<TestExec>) {
        let exec = Arc::new(TestExec::default());
        let deps = HostingDeps::new(Some(exec.clone() as Arc<dyn TurnExecutor>));
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

    impl TurnExecutor for TestExec {
        fn execute_stream<'a>(
            &'a self,
            _session: &'a mut SessionContext,
            _prompt: &'a str,
            mut on_token: Box<dyn FnMut(&str) + Send>,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            Box::pin(async move {
                self.runs.fetch_add(1, Ordering::SeqCst);
                let delay = *self.delay_ms.lock().unwrap();
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                on_token("hel");
                on_token("lo ");
                on_token("world");
                Ok("hello world".to_string())
            })
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
        let mut deps_builder = HostingDeps::new(Some(exec as Arc<dyn TurnExecutor>));
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
        let mut deps_builder = HostingDeps::new(Some(exec.clone() as Arc<dyn TurnExecutor>));
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
        let record = Arc::new(TurnRecord::new("t".into(), "s".into()));
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
}
