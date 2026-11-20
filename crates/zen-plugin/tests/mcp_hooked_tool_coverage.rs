//! W12.28 hook-coverage pins: the REAL [`HookedMcpTool`] wrapper path.
//!
//! # PURPOSE
//! Prove that an MCP-surface invocation dispatched through
//! `HookedMcpTool::invoke` traverses the full 5-hook pipeline
//! (confidentiality → budget → seatbelt → audit → approval) end-to-end —
//! not the hooks in isolation (those are pinned in their own modules).
//!
//! # USAGE
//! Run under nextest alongside the rest of the zen-plugin suite:
//! `cargo nextest run -p zen-plugin -E 'test(mcp_hooked_tool_coverage)'`.
//!
//! # EXPECTED
//! Two pins green:
//! - deny: an Ask-mode MUTATING tool (`fs.write`) invoked through the
//!   wrapper resolves the structured `approval-required` denial and an
//!   audit record lands with `"origin": "mcp"`;
//! - allow: a read-only tool (`fs.list`) invoked through the same wrapper
//!   shape succeeds and its audit record also carries `"origin": "mcp"`.
//!
//! # ERRORS
//! A regression in `hooks()` ordering, the `MCP_CALL` scope, or the
//! `with_origin("mcp")` tagging shows up as one of the assertions failing.

use std::path::PathBuf;
use std::sync::Arc;

use rig_compose::budget::AtomicBudget;
use rig_compose::tool::Tool;
use serde_json::{Value, json};
use zen_core::sandbox::{SandboxMode, SandboxValidator, SeatbeltPolicy};
use zen_core::types::Sensitivity;
use zen_plugin::mcp_hooked_tool::{HookedMcpTool, MCP_AUDIT_ORIGIN};
use zen_plugin::tools::fs_list::FsListTool;
use zen_plugin::tools::fs_write::FsWriteTool;

/// One shared construction seam: the wiring's `build_mcp_registry` shape
/// (shared unit-cost budget, declared sensitivity, Ask-mode seatbelt +
/// approval, explicit audit path) with a per-test tempdir audit file.
fn wrap(
    inner: Arc<dyn rig_compose::tool::Tool>,
    sensitivity: Sensitivity,
    audit_log_path: PathBuf,
    workspace_root: PathBuf,
) -> HookedMcpTool {
    HookedMcpTool::new(
        inner,
        sensitivity,
        Arc::new(AtomicBudget::new(20)),
        SeatbeltPolicy::new(SandboxMode::Ask, vec![workspace_root.clone()]),
        audit_log_path,
        SandboxMode::Ask,
    )
}

/// Parse every line of the audit JSONL and return the records.
fn audit_records(path: &PathBuf) -> Vec<Value> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("audit file must exist after invoke: {e}"));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("audit line must be valid JSON"))
        .collect()
}

/// W12.28 deny-path pin: a MUTATING tool (`fs.write`) invoked through
/// `HookedMcpTool::invoke` under Ask mode must resolve the structured
/// `approval-required` denial (MCP_CALL is scoped INSIDE the wrapper — the
/// test never scopes it itself) AND an audit record must land tagged with
/// the wrapper's `MCP_AUDIT_ORIGIN`.
///
/// Hook-coverage note: the approval hook is the LAST entry in the wrapper's
/// `hooks()` vec — reaching its `Terminate` proves ConfidentialityHook,
/// DispatchBudgetHook, SeatbeltHook and ToolAuditHook all observed
/// `before_invocation` first (the dispatch loop runs hooks in order and
/// breaks at the first non-Continue). One wrapper-level pin therefore
/// covers the whole pipeline; no artificial per-hook failures are needed.
#[tokio::test]
async fn deny_mutating_through_wrapper_surfaces_approval_required_and_audits_mcp_origin() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let audit_path = workspace.join("audit.jsonl");
    let target = workspace.join("denied.txt");

    let inner: Arc<dyn rig_compose::tool::Tool> = Arc::new(FsWriteTool::new(
        SandboxValidator::new(SandboxMode::Ask, vec![workspace.clone()]),
    ));
    let hooked = wrap(inner, Sensitivity::Private, audit_path.clone(), workspace);

    let err = hooked
        .invoke(json!({
            "path": target.to_string_lossy(),
            "content": "must never land"
        }))
        .await
        .expect_err("Ask-mode mutating MCP call must deny");

    let msg = err.to_string();
    assert!(
        msg.contains("approval-required"),
        "structured denial must surface through the wrapper: {msg}"
    );
    assert!(
        msg.contains("mcp:"),
        "denial must name the mcp origin: {msg}"
    );
    assert!(
        !target.exists(),
        "denial must happen BEFORE the tool body runs"
    );

    // Audit record written by the ToolAuditHook observed the Terminate —
    // success=false, origin tagged by the wrapper's with_origin("mcp").
    let records = audit_records(&audit_path);
    assert_eq!(records.len(), 1, "exactly one denial record: {records:?}");
    let rec = &records[0];
    assert_eq!(rec["tool"], "fs.write");
    assert_eq!(rec["success"], false, "record: {rec}");
    assert_eq!(
        rec["origin"], MCP_AUDIT_ORIGIN,
        "origin marker must be the wrapper's constant: {rec}"
    );
    assert!(
        rec["error"]
            .as_str()
            .is_some_and(|e| e.contains("approval-required")),
        "record must carry the structured denial: {rec}"
    );
}

/// W12.28 allow-path pin: a read-only tool (`fs.list`) invoked through the
/// same wrapper shape (all five hooks active, Ask mode, MCP_CALL scoped by
/// the wrapper) SUCCEEDS — proving budget reservation, seatbelt validation,
/// confidentiality gating and the approval bypass for non-mutating tools all
/// pass end-to-end — and its audit record carries `"origin": "mcp"`,
/// proving the pipeline ran (not a shortcut around it).
#[tokio::test]
async fn allow_readonly_through_wrapper_succeeds_and_audits_mcp_origin() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let listing = workspace.join("listing");
    std::fs::create_dir_all(&listing).unwrap();
    std::fs::write(listing.join("marker.txt"), "x").unwrap();
    let audit_path = workspace.join("audit.jsonl");

    let inner: Arc<dyn rig_compose::tool::Tool> = Arc::new(FsListTool::new(SandboxValidator::new(
        SandboxMode::Ask,
        vec![workspace.clone()],
    )));
    let hooked = wrap(inner, Sensitivity::Public, audit_path.clone(), workspace);

    let out = hooked
        .invoke(json!({ "path": listing.to_string_lossy() }))
        .await
        .expect("read-only MCP call must succeed through the pipeline");

    let entries = out["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("fs.list must return entries, got: {out}"));
    assert!(
        entries.iter().any(|e| e["name"] == "marker.txt"),
        "listed dir must contain the marker file, got: {out}"
    );

    let records = audit_records(&audit_path);
    assert_eq!(records.len(), 1, "exactly one success record: {records:?}");
    let rec = &records[0];
    assert_eq!(rec["tool"], "fs.list");
    assert_eq!(rec["success"], true, "record: {rec}");
    assert_eq!(
        rec["origin"], MCP_AUDIT_ORIGIN,
        "origin marker must be the wrapper's constant: {rec}"
    );
}
