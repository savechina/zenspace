//! `admission.list` — the read-only agent surface over the admission trust
//! ledger (007 T014, US4/FR-009; contracts/agent-tools.md).
//!
//! PURPOSE: let the agent answer "what would-accepts are pending?" /
//! "what did the channel ask for this week?" from the SAME `state.db`
//! `AdmissionRepo` the `zen admission` CLI reads.
//! USAGE: registered once by [`crate::ZenWiring`], granted to Sisyphus by the
//! orchestrator push list; the model calls it with `{status?, limit?}`.
//! READ-ONLY: there is deliberately no write op — mode flips and mandate
//! withdrawal are CLI-only (host-enforced; agent text can never change trust
//! state). Public sensitivity; the quality gate treats it as read-only.
//!
//! ERRORS: a missing ledger, unresolvable path, or SQLite failure is returned
//! as a structured `Ok({error})` payload so the round continues and the model
//! can adapt (never a `KernelError` batch abort).

use rig_compose::registry::KernelError;
use rig_compose::tool::{Tool, ToolSchema};
use serde_json::json;
use std::path::PathBuf;

use zen_repo::{AdmissionRepo, DecisionRow, DispositionFilter};

/// Tool name (the contract's `admission.list`).
pub const ADMISSION_LIST_TOOL_NAME: &str = "admission.list";

/// Default page size for an unfiltered read.
pub const DEFAULT_LIMIT: i64 = 20;
/// Lower clamp bound.
pub const MIN_LIMIT: i64 = 1;
/// Upper clamp bound.
pub const MAX_LIMIT: i64 = 100;

/// The decision vocabulary of record (migration 016).
pub const DISPOSITIONS: [&str; 5] = [
    "would_accept",
    "rejected",
    "admitted",
    "unresolved",
    "duplicate",
];

/// One rendered ledger row — the shape the CLI list surface renders.
fn render_row(row: &DecisionRow) -> serde_json::Value {
    json!({
        "id": row.id,
        "source": row.source,
        "disposition": row.disposition,
        "sender": row.sender,
        "snippet": row.content,
        "matched_rule": row.matched_rule,
        "scope": row.scope,
        "mode": row.accept_mode,
        "created_at": row.created_at,
        "reason": row.reason,
    })
}

/// The read-only admission ledger surface.
#[derive(Clone)]
pub struct AdmissionListTool {
    state_db: Option<PathBuf>,
}

impl AdmissionListTool {
    /// Create with the resolved `state.db` path (see
    /// [`crate::plan_todos::resolve_state_db`]; `None` degrades to an honest
    /// empty result, never a panic).
    pub fn new(state_db: Option<PathBuf>) -> Self {
        Self { state_db }
    }

    fn parse_status(args: &serde_json::Value) -> Result<Option<String>, String> {
        match args.get("status") {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(status)) => {
                if DISPOSITIONS.contains(&status.as_str()) {
                    Ok(Some(status.clone()))
                } else {
                    Err(format!(
                        "status {status:?} is outside the vocabulary {DISPOSITIONS:?}"
                    ))
                }
            }
            Some(_) => Err("status must be a string".to_string()),
        }
    }

    fn parse_limit(args: &serde_json::Value) -> Result<i64, String> {
        match args.get("limit") {
            None | Some(serde_json::Value::Null) => Ok(DEFAULT_LIMIT),
            Some(value) => value
                .as_i64()
                .map(|limit| limit.clamp(MIN_LIMIT, MAX_LIMIT))
                .ok_or_else(|| "limit must be an integer".to_string()),
        }
    }

    async fn list(
        &self,
        status: Option<String>,
        limit: i64,
    ) -> Result<serde_json::Value, KernelError> {
        let Some(path) = self.state_db.as_ref() else {
            return Ok(json!({
                "rows": [],
                "count": 0,
                "status": status,
                "limit": limit,
                "note": "admission ledger unavailable: state.db path unresolvable",
            }));
        };
        // The brief worker discipline: never CREATE the daemon's database as
        // a side effect of a read-only query.
        if !path.exists() {
            return Ok(json!({
                "rows": [],
                "count": 0,
                "status": status,
                "limit": limit,
                "note": "no admission records yet (state.db not present)",
            }));
        }
        let client = match zen_repo::SqliteClient::open(path).await {
            Ok(client) => client,
            Err(e) => {
                return Ok(json!({
                    "error": format!("admission ledger unavailable: {e}"),
                    "admission.list": true,
                }));
            }
        };
        let repo = AdmissionRepo::new(&client);
        let filter = DispositionFilter {
            disposition: status.clone(),
            since: None,
        };
        match repo.list_decisions(filter, limit).await {
            Ok(rows) => {
                let rendered = rows.iter().map(render_row).collect::<Vec<_>>();
                Ok(json!({
                    "rows": rendered,
                    "count": rendered.len(),
                    "status": status,
                    "limit": limit,
                }))
            }
            Err(e) => Ok(json!({
                "error": format!("admission ledger read failed: {e}"),
                "admission.list": true,
            })),
        }
    }
}

#[async_trait::async_trait]
impl Tool for AdmissionListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ADMISSION_LIST_TOOL_NAME.to_string(),
            description: "Read the admission trust ledger (read-only): decisions the \
              inbound channel gate recorded — would_accept (observe-mode rehearsals), \
              rejected, admitted (executed work), unresolved (fail-closed), duplicate \
              (replayed triggers). Same store `zen admission list` reads. Mode flips and \
              mandate withdrawal are CLI-only; this tool never changes trust state."
                .to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": DISPOSITIONS,
                        "description": "optional disposition filter"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": MIN_LIMIT,
                        "maximum": MAX_LIMIT,
                        "description": "page size (default 20, clamped 1..=100)"
                    }
                }
            }),
            result_schema: json!({
                "type": "object",
                "properties": {
                    "rows": { "type": "array" },
                    "count": { "type": "integer" },
                    "status": { "type": ["string", "null"] },
                    "limit": { "type": "integer" },
                    "note": { "type": "string" },
                    "error": { "type": "string" }
                }
            }),
        }
    }

    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, KernelError> {
        let status = match Self::parse_status(&args) {
            Ok(status) => status,
            Err(e) => return Ok(json!({ "error": e, "admission.list": true })),
        };
        let limit = match Self::parse_limit(&args) {
            Ok(limit) => limit,
            Err(e) => return Ok(json!({ "error": e, "admission.list": true })),
        };
        self.list(status, limit).await
    }
}
