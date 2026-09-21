#![allow(dead_code)] // T079 approval types — fields/methods are API surface for rendering + sink wiring
//! Approval popup state and rendering (FR-024, T079).
//!
//! PURPOSE: Manages the bottom-pane approval popup when the gateway routes
//!   an approval request (Q3) to this surface. Renders action details,
//!   pauses input, and transmits decisions back on the originating turn.
//!
//! USAGE: The `App` holds an `ApprovalState` field. When a Q3 request arrives,
//!   it is queued. The inline_ui render shows the front request as a popup.
//!   Key handling in inline_handler.rs routes y/n/Esc to decision logic.
//!
//! EXPECTED: One approval popup visible at a time (FIFO). Input is paused
//!   while pending. y/Y = approve, n/N/Esc = deny. Timeout/withdrawal closes
//!   popup + shows toast.
//!
//! ERRORS: None — pure state type. Decision transmission is async.

use std::collections::VecDeque;
use std::time::Instant;

/// Duration (seconds) before an approval popup auto-closes with a timeout notice.
/// This is the CLIENT-SIDE rendering timeout. The gateway-side watchdog
/// (`-32011 approval-timeout`, 120s) is the authoritative deadline.
/// We use a shorter rendering timeout so the popup closes visibly before
/// the gateway cancels the turn.
const APPROVAL_RENDER_TIMEOUT_SECS: u64 = 120;

/// A pending approval request from the gateway (Q3).
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The turn ID that originated this request (for turn-affinity routing).
    pub turn_id: String,
    /// The server-assigned request ID (for responding).
    pub request_id: String,
    /// Tool name (e.g. "shell.exec", "fs.write").
    pub tool_name: String,
    /// Tool invocation details (binary+args for shell.exec, path for fs.*).
    pub invocation: serde_json::Value,
    /// Human-readable reason for the approval.
    pub reason: String,
    /// When this request was received (for timeout tracking).
    pub received_at: Instant,
}

impl ApprovalRequest {
    /// Render the action details for the popup. Shows binary+args for
    /// shell.exec, path for fs.*, generic tool name + JSON args otherwise.
    /// Truncated to fit within popup row budget.
    pub fn detail_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!("Tool: {}", self.tool_name));

        match self.tool_name.as_str() {
            "shell.exec" => {
                if let Some(binary) = self.invocation.get("binary").and_then(|v| v.as_str()) {
                    lines.push(format!("Binary: {binary}"));
                }
                if let Some(args) = self.invocation.get("args").and_then(|v| v.as_array()) {
                    let args_str: Vec<&str> = args.iter().filter_map(|a| a.as_str()).collect();
                    if !args_str.is_empty() {
                        let joined = args_str.join(" ");
                        // Truncate to 60 chars to fit popup
                        if joined.len() > 60 {
                            lines.push(format!("Args: {}...", &joined[..57]));
                        } else {
                            lines.push(format!("Args: {joined}"));
                        }
                    }
                }
            }
            name if name.starts_with("fs.") => {
                if let Some(path) = self.invocation.get("path").and_then(|v| v.as_str()) {
                    lines.push(format!("Path: {path}"));
                } else {
                    let args_str = serde_json::to_string(&self.invocation).unwrap_or_default();
                    if args_str.len() > 60 {
                        lines.push(format!("Args: {}...", &args_str[..57]));
                    } else if args_str != "null" {
                        lines.push(format!("Args: {args_str}"));
                    }
                }
            }
            _ => {
                let args_str = serde_json::to_string(&self.invocation).unwrap_or_default();
                if args_str.len() > 60 {
                    lines.push(format!("Args: {}...", &args_str[..57]));
                } else if args_str != "null" {
                    lines.push(format!("Args: {args_str}"));
                }
            }
        }

        if !self.reason.is_empty() {
            lines.push(format!("Reason: {}", self.reason));
        }
        lines
    }

    /// Render the popup as a list of styled lines for the viewport.
    /// Budget: title line + detail lines + action hint = ≤6 rows total.
    pub fn popup_lines(&self) -> Vec<String> {
        let mut lines = vec!["--- Approval Required ---".to_string()];
        lines.extend(self.detail_lines());
        lines.push("[y] Approve  [n/Esc] Deny".to_string());
        // Ensure we never exceed 6 rows
        lines.truncate(6);
        lines
    }

    /// Whether the approval has timed out (client-side rendering timeout).
    pub fn is_timed_out(&self) -> bool {
        self.received_at.elapsed().as_secs() >= APPROVAL_RENDER_TIMEOUT_SECS
    }
}

/// Decision transmitted back for an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

/// State of the approval popup system (FR-024).
///
/// Multiple queued approvals present ONE AT A TIME, in arrival order (FIFO).
/// The popup is visible only when `current` is `Some`. Input is paused
/// while `current` is `Some`.
#[derive(Debug, Default)]
pub struct ApprovalState {
    /// The currently visible approval request, if any.
    pub current: Option<ApprovalRequest>,
    /// FIFO queue of pending approval requests behind the current one.
    pub queue: VecDeque<ApprovalRequest>,
}

impl ApprovalState {
    /// Push a new approval request. If no popup is showing, it becomes
    /// the current (visible) request immediately.
    pub fn push(&mut self, request: ApprovalRequest) {
        if self.current.is_none() {
            self.current = Some(request);
        } else {
            self.queue.push_back(request);
        }
    }

    /// Resolve the current approval with a decision. Returns the request
    /// and decision for transmission, and promotes the next queued request
    /// if any.
    pub fn resolve_current(
        &mut self,
        decision: ApprovalDecision,
    ) -> Option<(ApprovalRequest, ApprovalDecision)> {
        let request = self.current.take()?;
        // Promote next queued request
        self.current = self.queue.pop_front();
        Some((request, decision))
    }

    /// Close the current approval (e.g. on timeout or withdrawal) without
    /// a decision. Promotes the next queued request.
    pub fn close_current(&mut self) -> Option<ApprovalRequest> {
        let request = self.current.take()?;
        self.current = self.queue.pop_front();
        Some(request)
    }

    /// Whether an approval popup is currently visible (input should be paused).
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.current.is_some()
    }

    /// Number of pending approvals (current + queued).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        let queued = self.queue.len();
        if self.current.is_some() {
            queued + 1
        } else {
            queued
        }
    }

    /// Check if the current request has timed out and close it if so.
    /// Returns the timed-out request for notice rendering.
    pub fn check_timeout(&mut self) -> Option<ApprovalRequest> {
        if let Some(ref current) = self.current
            && current.is_timed_out()
        {
            return self.close_current();
        }
        None
    }

    /// Clear all pending approvals (e.g. on disconnect).
    pub fn clear(&mut self) {
        self.current = None;
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(tool: &str) -> ApprovalRequest {
        ApprovalRequest {
            turn_id: "turn-1".to_string(),
            request_id: "req-1".to_string(),
            tool_name: tool.to_string(),
            invocation: serde_json::json!({"binary": "/bin/sh", "args": ["-c", "echo hello"]}),
            reason: "test".to_string(),
            received_at: Instant::now(),
        }
    }

    #[test]
    fn single_request_becomes_current() {
        let mut state = ApprovalState::default();
        let req = make_request("shell.exec");
        state.push(req);
        assert!(state.is_pending());
        assert_eq!(state.pending_count(), 1);
        assert!(state.current.is_some());
        assert!(state.queue.is_empty());
    }

    #[test]
    fn second_request_goes_to_queue() {
        let mut state = ApprovalState::default();
        state.push(make_request("shell.exec"));
        state.push(make_request("fs.write"));
        assert_eq!(state.pending_count(), 2);
        assert_eq!(state.current.as_ref().unwrap().tool_name, "shell.exec");
        assert_eq!(state.queue.len(), 1);
    }

    #[test]
    fn resolve_promotes_next() {
        let mut state = ApprovalState::default();
        state.push(make_request("shell.exec"));
        state.push(make_request("fs.write"));

        let (req, decision) = state.resolve_current(ApprovalDecision::Approve).unwrap();
        assert_eq!(req.tool_name, "shell.exec");
        assert_eq!(decision, ApprovalDecision::Approve);
        assert_eq!(state.current.as_ref().unwrap().tool_name, "fs.write");
        assert!(state.queue.is_empty());
    }

    #[test]
    fn close_promotes_next() {
        let mut state = ApprovalState::default();
        state.push(make_request("shell.exec"));
        state.push(make_request("fs.write"));

        let req = state.close_current().unwrap();
        assert_eq!(req.tool_name, "shell.exec");
        assert_eq!(state.current.as_ref().unwrap().tool_name, "fs.write");
    }

    #[test]
    fn fifo_order() {
        let mut state = ApprovalState::default();
        state.push(make_request("a"));
        state.push(make_request("b"));
        state.push(make_request("c"));

        let (req, _) = state.resolve_current(ApprovalDecision::Approve).unwrap();
        assert_eq!(req.tool_name, "a");
        let (req, _) = state.resolve_current(ApprovalDecision::Deny).unwrap();
        assert_eq!(req.tool_name, "b");
        let (req, _) = state.resolve_current(ApprovalDecision::Approve).unwrap();
        assert_eq!(req.tool_name, "c");
    }

    #[test]
    fn clear_empties_all() {
        let mut state = ApprovalState::default();
        state.push(make_request("a"));
        state.push(make_request("b"));
        state.clear();
        assert!(!state.is_pending());
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn detail_lines_shell_exec() {
        let req = make_request("shell.exec");
        let lines = req.detail_lines();
        assert!(lines.iter().any(|l| l.contains("shell.exec")));
        assert!(lines.iter().any(|l| l.contains("/bin/sh")));
    }

    #[test]
    fn detail_lines_fs_tool() {
        let mut req = make_request("fs.write");
        req.invocation = serde_json::json!({"path": "/tmp/test.txt"});
        let lines = req.detail_lines();
        assert!(lines.iter().any(|l| l.contains("/tmp/test.txt")));
    }

    #[test]
    fn popup_lines_truncated_to_six() {
        let mut req = make_request("shell.exec");
        req.reason = "a".repeat(200);
        let lines = req.popup_lines();
        assert!(lines.len() <= 6);
    }
}

/// Response sent back to the gateway pump for an approval decision.
#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    /// The server-assigned request ID (for responding).
    pub request_id: String,
    /// The turn ID that originated this request (for turn-affinity routing).
    pub turn_id: String,
    /// The decision made by the user.
    pub decision: ApprovalDecision,
}
