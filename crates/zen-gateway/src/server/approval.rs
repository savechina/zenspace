//! Approval routing (T030, FR-010) — bridges the synchronous sandbox
//! approval callback into asynchronous Q3 `approval/request` round-trips
//! routed to the originating connection only (SC-007).
//!
//! PURPOSE: The daemon's shared [`AgentOrchestrator`] runs with
//! `SandboxMode::Ask` and ONE process-wide approval callback ([`Self::
//! callback`], installed via `with_approval_callback`). That callback
//! fires deep inside tool dispatch on a blocking thread with no turn
//! context (`ToolInvocation` carries only name+args), so this broker
//! correlates invocations to hosted turns by claim protocol: each
//! registered turn claims the callback while it awaits its own decision,
//! giving unambiguous routing without touching rig-compose types.
//!
//! USAGE: The daemon creates one broker, passes [`Self::callback`] to
//! `AgentOrchestrator::with_approval_callback`, and hands clones to
//! [`crate::server::hosting::SessionHost`]; hosted turns
//! [`Self::register`]/[`Self::deregister`] around execution. Each
//! registration spawns an async worker performing the Q3 round-trip
//! through the origin connection's [`ConnectionHandle`].
//!
//! EXPECTED: approve→Allow / anything-else→Deny; Q3 deadline miss
//! (-32011) cancels the turn and audits; carriers that negotiated
//! `approvals:false` surface -32010 as a denial with an audit line.
//! Unrouted callbacks (no active hosted turn) deny immediately —
//! non-hosted stacks sharing the orchestrator are never prompted.
//!
//! ERRORS: callback never panics across the bridge; all failures decay
//! to `Deny` (fail-safe) plus audit.

use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};

use crate::protocol::RpcErrorBody;
use crate::server::dispatch::{APPROVAL_TIMEOUT, ConnectionHandle};

/// Broker-side deadline; slightly under the sandbox hook's own 120s so
/// the timeout path (audit + cancel) executes inside our control.
const BROKER_DEADLINE: Duration = APPROVAL_TIMEOUT.saturating_sub(Duration::from_secs(1));

/// One hosted turn's approval route.
struct TurnRoute {
    turn_id: String,
    /// Jobs awaiting an async Q3 round-trip.
    jobs: tokio::sync::mpsc::UnboundedSender<ApprovalJob>,
    /// True while the sync callback has claimed this route.
    claimed: AtomicBool,
}

struct ApprovalJob {
    invocation: Value,
    reason: String,
    reply: std::sync::mpsc::SyncSender<bool>,
}

/// Process-wide approval router (see module docs).
#[derive(Default)]
pub struct ApprovalBroker {
    routes: StdMutex<Vec<Arc<TurnRoute>>>,
}

impl ApprovalBroker {
    /// The single callback installed on the daemon orchestrator. The
    /// invocation parameter's type is `&ToolInvocation` (rig-compose);
    /// it stays unnameable here by design, so fields are read through
    /// inference and only primitives cross the broker boundary.
    pub fn callback(self: &Arc<Self>) -> zen_core::sandbox::ApprovalCallback {
        let broker = Arc::clone(self);
        Arc::new(move |invocation: &_| {
            let name = invocation.name.to_string();
            let args = invocation.args.clone();
            if broker.decide(name, args) {
                zen_core::sandbox::ApprovalDecision::Allow
            } else {
                zen_core::sandbox::ApprovalDecision::Deny
            }
        })
    }

    /// Registers a hosted turn's route and spawns its async worker.
    /// Returns a deregistration guard.
    pub fn register(
        self: &Arc<Self>,
        turn_id: String,
        connection: ConnectionHandle,
        on_timeout_cancel: impl FnOnce() + Send + 'static,
    ) -> DeregisterGuard {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalJob>();
        let route = Arc::new(TurnRoute {
            turn_id: turn_id.clone(),
            jobs: tx,
            claimed: AtomicBool::new(false),
        });
        self.routes
            .lock()
            .expect("routes lock")
            .push(Arc::clone(&route));

        // The worker must own only clones: holding the route Arc here
        // would keep the `jobs` sender alive from inside the receiver's
        // own task, so `rx.recv()` would never close and every hosted
        // turn would leak the worker plus its ConnectionHandle.
        tokio::spawn(async move {
            let mut cancel_on_exit = Some(on_timeout_cancel);
            while let Some(job) = rx.recv().await {
                match connection
                    .request_approval_with_timeout(
                        &turn_id,
                        job.invocation,
                        &job.reason,
                        BROKER_DEADLINE,
                    )
                    .await
                {
                    Ok(result) => {
                        let approve = result["decision"] == json!("approve");
                        let _ = job.reply.send(approve);
                    }
                    Err(err) => {
                        tracing::warn!(
                            turn = %turn_id,
                            code = err.code,
                            "approval request failed: {}",
                            err.message
                        );
                        if err.code == -32011 {
                            // Deadline miss: cancel the turn (contracts/02).
                            if let Some(cancel) = cancel_on_exit.take() {
                                cancel();
                            }
                        }
                        let _ = job.reply.send(false);
                    }
                }
            }
        });

        DeregisterGuard {
            broker: Arc::clone(self),
            route,
        }
    }

    /// Routes one invocation through the claim protocol without
    /// requiring the concrete sandbox invocation type (inspection/test
    /// entry point; production traffic enters via [`Self::callback`]).
    pub fn route(&self, name: String, args: Value) -> bool {
        self.decide(name, args)
    }

    /// Sync entry point (runs on the sandbox hook's blocking thread):
    /// claims a free route, performs a blocking round-trip bounded by
    /// [`BROKER_DEADLINE`], and decays every failure to `Deny`.
    fn decide(&self, name: String, args: Value) -> bool {
        let route = {
            let routes = self.routes.lock().expect("routes lock");
            let idx = routes
                .iter()
                .position(|r| !r.claimed.load(Ordering::SeqCst));
            let Some(idx) = idx else {
                // No hosted context: fail-safe deny.
                return false;
            };
            let route = Arc::clone(&routes[idx]);
            route.claimed.store(true, Ordering::SeqCst);
            route
        };

        let outcome = self.blocking_round_trip(&route, name, args);
        route.claimed.store(false, Ordering::SeqCst);
        outcome
    }

    fn blocking_round_trip(&self, route: &Arc<TurnRoute>, name: String, args: Value) -> bool {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let payload = json!({ "name": name, "args": args });
        let job = ApprovalJob {
            invocation: payload,
            reason: format!("hosted turn requires approval for {name}"),
            reply: reply_tx,
        };
        if route.jobs.send(job).is_err() {
            return false;
        }
        reply_rx
            .recv_timeout(BROKER_DEADLINE + Duration::from_secs(5))
            .unwrap_or_default()
    }

    fn deregister(&self, route: &TurnRoute) {
        let mut routes = self.routes.lock().expect("routes lock");
        routes.retain(|r| r.turn_id != route.turn_id);
    }
}

/// Removes the route when dropped (normal turn finalization or abort).
pub struct DeregisterGuard {
    broker: Arc<ApprovalBroker>,
    route: Arc<TurnRoute>,
}

impl Drop for DeregisterGuard {
    fn drop(&mut self) {
        self.broker.deregister(&self.route);
    }
}

/// Maps a broker-level [`RpcError`] body for turn_error events.
pub fn error_body(code: i32, message: &str) -> RpcErrorBody {
    RpcErrorBody {
        code,
        name: "approval-failed".into(),
        message: message.into(),
        data: None,
    }
}
