//! W6.5 (D21 §5): final-answer assembly for plan turns.
//!
//! A bounded non-streaming LLM synthesis over EVERY task outcome (SC-012:
//! no task dropped, slot order), followed by a deterministic `Plan result`
//! block. The pass runs after the quality gate and never re-judges it;
//! failure, timeout or the substantive-answer skip (F4) degrade to the
//! in-loop prose + block — a turn never fails because synthesis failed.
//!
//! Zero config keys (约定大于配置): every budget is a code-tunable constant.
//! A turn whose dispatch results contain no `plan.execute` batch emits zero
//! extra tokens and never calls synthesis.

use serde_json::Value;

/// One-line summary cap for the result-block rows (shared with the archive
/// page writer — SC-010 renders the same rows).
pub use zen_vault::distill::reflection::PLAN_RESULT_SUMMARY_CHARS;

/// Per-task excerpt cap in the synthesis prompt (SC-012).
pub const SYNTHESIS_EXCERPT_CHARS: usize = 1200;

/// Total synthesis-input cap across all task excerpts (SC-012). The cap is
/// spread evenly — every task keeps an excerpt, none is dropped.
pub const SYNTHESIS_MAX_CHARS: usize = 8000;

/// F4: an in-loop answer at least this long is already substantive — the
/// synthesis call is skipped (`synthesis: skipped:sufficient`).
pub const SYNTHESIS_SUFFICIENT_CHARS: usize = 2000;

/// Wall-clock cap for a cloud-routed synthesis call (SC-012, /plan-eng-review
/// D18-A): one-size timeouts are dead on local models, but a cloud call must
/// still be bounded independently of the first-token budget.
pub const SYNTHESIS_TIMEOUT_CLOUD_SECS: u64 = 60;

/// Wall-clock cap for a local-routed synthesis call: `min(300, first-token
/// const)` — the same "one budget derives another" pattern the primary
/// timeout canonizes (D22).
pub const SYNTHESIS_TIMEOUT_LOCAL_CAP_SECS: u64 = 300;

/// One task's outcome, in slot order, as it reached the final answer.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOutcomeRow {
    pub id: String,
    pub agent: String,
    pub status: String,
    /// Full task response (or the failure reason) — excerpted per budget.
    pub response: String,
}

/// Everything the synthesis + block need from one dispatched batch.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOutcomeSnapshot {
    pub plan_id: String,
    pub rows: Vec<PlanOutcomeRow>,
    pub plan_approved: bool,
    pub delivery_ready: bool,
    pub archived: Option<String>,
    /// W9.4: triage proposals for recognized failure classes; empty when
    /// none matched (the block section is then omitted byte-identically).
    pub triage: Vec<String>,
}

/// Everything one synthesis decision needs from the turn. Bundled so the
/// runner stays under the argument-count lint limit.
pub struct SynthesisCall<'a> {
    pub sensitivity: zen_core::types::Sensitivity,
    pub session_id: &'a str,
    pub agent_name: &'a str,
    pub user_query: &'a str,
    pub final_prose: &'a str,
    pub snapshot: &'a PlanOutcomeSnapshot,
    pub vetoed: bool,
}

/// Pure decision over the turn state: does the synthesis pass run, and if
/// not, why (rendered verbatim as `synthesis: skipped:<reason>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynthesisDecision {
    Run,
    Skip { reason: &'static str },
}

pub fn decide_synthesis(batch_ran: bool, vetoed: bool, final_prose: &str) -> SynthesisDecision {
    if !batch_ran {
        // No batch ⇒ zero extra tokens and zero synthesis calls (SC-012).
        SynthesisDecision::Skip { reason: "no_batch" }
    } else if vetoed {
        // The gate judged the in-loop turn not ready; synthesizing a
        // polished answer would contradict the veto. Block only.
        SynthesisDecision::Skip { reason: "vetoed" }
    } else if final_prose.chars().count() >= SYNTHESIS_SUFFICIENT_CHARS {
        // F4: the in-loop answer is already substantive.
        SynthesisDecision::Skip {
            reason: "sufficient",
        }
    } else {
        SynthesisDecision::Run
    }
}

/// Wall-clock cap for the synthesis call, derived from the route: cloud is
/// flat, local derives from the first-token budget.
pub fn synthesis_timeout_secs(is_local: bool) -> u64 {
    if is_local {
        SYNTHESIS_TIMEOUT_LOCAL_CAP_SECS.min(crate::completion_model::first_token_budget_secs())
    } else {
        SYNTHESIS_TIMEOUT_CLOUD_SECS
    }
}

/// SC-012: every task reaches the synthesis — the total budget is spread
/// evenly, so a 12-task plan beyond any history cap still feeds all 12
/// outcomes.
pub fn per_task_excerpt_budget(rows: usize) -> usize {
    if rows == 0 {
        return SYNTHESIS_EXCERPT_CHARS;
    }
    let share = SYNTHESIS_MAX_CHARS / rows;
    let budget = share.saturating_sub(SYNTHESIS_LINE_OVERHEAD);
    budget.clamp(1, SYNTHESIS_EXCERPT_CHARS)
}

/// Fixed per-row cost of a `- id=… agent=… status=…: ` prompt line; the
/// excerpt budget subtracts it so the total stays within the cap.
pub const SYNTHESIS_LINE_OVERHEAD: usize = 48;

/// Flatten a raw response into one bounded line (newline-free, capped).
fn one_line(text: &str, cap: usize) -> String {
    text.replace(['\n', '\r'], " ")
        .trim()
        .chars()
        .take(cap)
        .collect()
}

/// Build the synthesis prompt: the user request plus every task outcome in
/// slot order, excerpts bounded by [`per_task_excerpt_budget`], total
/// bounded by [`SYNTHESIS_MAX_CHARS`].
pub fn build_synthesis_prompt(user_request: &str, rows: &[PlanOutcomeRow]) -> String {
    let budget = per_task_excerpt_budget(rows.len());
    let mut prompt = format!(
        "Compose the final answer for the user's request. You are given every \
         task outcome of a completed plan, in execution order. Write a \
         coherent, self-contained answer for the user — do not restate the \
         task table.\n\nUser request:\n{}\n\nTask outcomes:\n",
        one_line(user_request, SYNTHESIS_EXCERPT_CHARS)
    );
    for row in rows {
        prompt.push_str(&format!(
            "- id={} agent={} status={}: {}\n",
            row.id,
            row.agent,
            row.status,
            one_line(&row.response, budget)
        ));
    }
    prompt
}

/// Deterministic `Plan result` block — pure formatting, no LLM, never
/// omitted when a batch ran. `synthesis_label` is the honesty line
/// (`ok` | `skipped:<reason>` | `failed`).
pub fn render_plan_result_block(snapshot: &PlanOutcomeSnapshot, synthesis_label: &str) -> String {
    render_plan_result_block_in(
        zen_core::types::PlanResultFormat::MarkdownTable,
        snapshot,
        synthesis_label,
    )
}

/// W8.2: one builder, two renderings chosen server-side by surface kind.
/// Both carry every task in slot order (failed/skipped included); the
/// plain rendering never emits a raw table pipe.
pub fn render_plan_result_block_in(
    format: zen_core::types::PlanResultFormat,
    snapshot: &PlanOutcomeSnapshot,
    synthesis_label: &str,
) -> String {
    let mut block = String::from("Plan result\n");
    match format {
        zen_core::types::PlanResultFormat::MarkdownTable => {
            block.push_str("| id | agent | status | summary |\n");
            block.push_str("|----|-------|--------|---------|\n");
            for row in &snapshot.rows {
                block.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    table_cell(&row.id),
                    table_cell(&row.agent),
                    table_cell(&row.status),
                    table_cell(&one_line(&row.response, PLAN_RESULT_SUMMARY_CHARS))
                ));
            }
        }
        zen_core::types::PlanResultFormat::PlainText => {
            for row in &snapshot.rows {
                let tag = match row.status.as_str() {
                    "completed" => "ok",
                    other => other,
                };
                block.push_str(&format!(
                    "- [{tag}] {} · {} · {}\n",
                    row.id,
                    row.agent,
                    plain_summary(&row.response)
                ));
            }
        }
    }
    block.push_str(&format!(
        "verdict: plan_approved={} delivery_ready={}\n",
        snapshot.plan_approved, snapshot.delivery_ready
    ));
    if let Some(archived) = &snapshot.archived {
        block.push_str(&format!("archived: {archived}\n"));
    }
    // W9.4: triage section — only for plans with recognized failure
    // classes; absent (byte-identical) otherwise. Text-only: nothing here
    // re-runs anything.
    for proposal in &snapshot.triage {
        block.push_str(&format!("triage: {proposal}\n"));
    }
    block.push_str(&format!("synthesis: {synthesis_label}\n"));
    block
}

/// Escape a markdown-table cell: table syntax inside task output must not
/// break the row shape.
fn table_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Plain-text summary: flattened and pipe-free — a `|` in task output
/// would masquerade as table syntax on text-only surfaces.
fn plain_summary(text: &str) -> String {
    one_line(text, PLAN_RESULT_SUMMARY_CHARS).replace('|', "/")
}

/// Extract the plan-outcome snapshot from a turn's dispatch results: the
/// `plan.execute` result carries the full per-task rows (id · agent ·
/// status · response), the Hermes verdict and the archive path — all
/// independent of the capped tool-results history.
pub fn extract_snapshot(
    results: &[rig_compose::normalizer::ToolInvocationResult],
) -> Option<PlanOutcomeSnapshot> {
    let plan_result = results
        .iter()
        .find(|r| r.invocation.name.as_str() == crate::plan_task::PLAN_TOOL_NAME)
        .map(|r| &r.output)?;
    let rows: Vec<PlanOutcomeRow> = plan_result
        .get("tasks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let id = t.get("id")?.as_str()?.to_string();
                    let agent = t
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let status = t
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("skipped")
                        .to_string();
                    let response = zen_core::sanitize::strip_user_content_markers(
                        t.get("response")
                            .and_then(|v| v.as_str())
                            .or_else(|| t.get("error").and_then(|v| v.as_str()))
                            .unwrap_or(""),
                    );
                    Some(PlanOutcomeRow {
                        id,
                        agent,
                        status,
                        response,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return None;
    }
    let hermes = plan_result.get("hermes").cloned().unwrap_or(Value::Null);
    Some(PlanOutcomeSnapshot {
        plan_id: plan_result
            .get("plan_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        rows,
        plan_approved: hermes
            .get("plan_approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        delivery_ready: hermes
            .get("delivery_ready")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        archived: plan_result
            .get("archived")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        triage: plan_result
            .get("triage")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// `Provider::Ollama | Mock` route on the local budget; everything else is
/// cloud (SC-012: local-routed calls get the derived local budget).
pub fn is_local_provider(provider: &zen_provider::Provider) -> bool {
    match provider {
        zen_provider::Provider::Ollama | zen_provider::Provider::Mock => true,
        // Config-declared providers resolve to `Unknown(name)` when they are
        // not one of the rig-native protocol variants — mock/ollama names
        // route locally.
        zen_provider::Provider::Unknown(name) => {
            matches!(name.as_str(), "mock" | "ollama")
        }
        _ => false,
    }
}

/// Audit-facing provider label — the inner name for `Unknown`, the variant
/// name otherwise.
pub fn provider_name(provider: &zen_provider::Provider) -> String {
    match provider {
        zen_provider::Provider::Unknown(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_compose::normalizer::{ToolInvocation, ToolInvocationResult};
    use serde_json::json;

    fn plan_result(tasks: Value, archived: Option<&str>) -> ToolInvocationResult {
        let mut out = json!({
            "plan": "demo",
            "plan_id": "p1",
            "tasks": tasks,
            "hermes": {"plan_approved": true, "delivery_ready": true},
        });
        if let Some(a) = archived {
            out["archived"] = json!(a);
        }
        ToolInvocationResult {
            invocation: ToolInvocation {
                name: crate::plan_task::PLAN_TOOL_NAME.to_string(),
                args: json!({}),
            },
            output: out,
        }
    }

    // SC-012 acceptance: outcomes beyond the capped tool-results history
    // still all reach the synthesis — extraction reads the plan result, not
    // the history.
    #[test]
    fn snapshot_extracts_every_task_row_in_slot_order() {
        let rows: Vec<Value> = (0..12)
            .map(|i| {
                json!({
                    "id": format!("t{i}"), "agent": "Explore",
                    "status": "completed", "response": format!("resp {i}")
                })
            })
            .collect();
        let results = vec![plan_result(json!(rows), None)];
        let snap = extract_snapshot(&results).expect("snapshot");
        assert_eq!(snap.rows.len(), 12);
        assert_eq!(snap.rows[0].id, "t0");
        assert_eq!(snap.rows[11].id, "t11");
        assert_eq!(snap.plan_id, "p1");
        assert!(snap.plan_approved && snap.delivery_ready);
    }

    #[test]
    fn snapshot_reads_archived_path_and_failure_reasons() {
        let results = vec![plan_result(
            json!([
                {"id": "a", "agent": "Explore", "status": "completed", "response": "ok"},
                {"id": "b", "agent": "Explore", "status": "failed", "error": "boom"}
            ]),
            Some("vault/wiki/wisdom/plans/demo.md"),
        )];
        let snap = extract_snapshot(&results).expect("snapshot");
        assert_eq!(
            snap.archived.as_deref(),
            Some("vault/wiki/wisdom/plans/demo.md")
        );
        assert_eq!(snap.rows[1].response, "boom");
    }

    #[test]
    fn snapshot_none_without_plan_batch() {
        let results = vec![ToolInvocationResult {
            invocation: ToolInvocation {
                name: "fs.read".to_string(),
                args: json!({}),
            },
            output: json!({"content": "x"}),
        }];
        assert!(extract_snapshot(&results).is_none());
    }

    // Phase 17 G-44 pin: models occasionally echo the executor's
    // [USER_CONTENT_START]/[USER_CONTENT_END] wrapping markers into task
    // responses; the Plan result block and the synthesis prompt consume
    // extract_snapshot rows and must render corpus-clean text (production:
    // wisdom/plans/t1.md carried the markers verbatim, 2026-11-22).
    #[test]
    fn snapshot_rows_strip_user_content_markers() {
        let results = vec![plan_result(
            json!([
                {"id": "t1", "agent": "Hermes", "status": "completed",
                 "response": "[USER_CONTENT_START]report body[USER_CONTENT_END]"},
                {"id": "t2", "agent": "Hermes", "status": "failed",
                 "error": "[USER_CONTENT_START]boom[USER_CONTENT_END]"}
            ]),
            None,
        )];
        let snap = extract_snapshot(&results).expect("snapshot");
        assert_eq!(snap.rows[0].response, "report body");
        assert_eq!(snap.rows[1].response, "boom");
    }

    // SC-012 acceptance: a no-batch turn never runs the pass.
    #[test]
    fn decide_no_batch_and_sufficient_skips() {
        assert_eq!(
            decide_synthesis(false, false, ""),
            SynthesisDecision::Skip { reason: "no_batch" }
        );
        let long = "x".repeat(SYNTHESIS_SUFFICIENT_CHARS);
        assert_eq!(
            decide_synthesis(true, false, &long),
            SynthesisDecision::Skip {
                reason: "sufficient"
            }
        );
        assert_eq!(
            decide_synthesis(true, true, ""),
            SynthesisDecision::Skip { reason: "vetoed" }
        );
        assert_eq!(
            decide_synthesis(true, false, "short"),
            SynthesisDecision::Run
        );
    }

    // SC-012 acceptance: the derived timeout — 60s cloud, local derives
    // from the first-token budget (capped at 300).
    #[test]
    fn timeout_derived_from_route() {
        assert_eq!(synthesis_timeout_secs(false), SYNTHESIS_TIMEOUT_CLOUD_SECS);
        let local = synthesis_timeout_secs(true);
        assert!(
            local <= SYNTHESIS_TIMEOUT_LOCAL_CAP_SECS
                && local <= crate::completion_model::first_token_budget_secs(),
            "local cap derives from the first-token budget: {local}"
        );
        assert_eq!(
            local,
            SYNTHESIS_TIMEOUT_LOCAL_CAP_SECS
                .min(crate::completion_model::first_token_budget_secs())
        );
    }

    // SC-012: 12-task plan — the excerpt budget spreads, no task dropped.
    #[test]
    fn prompt_bounded_but_complete_for_wide_plans() {
        let rows: Vec<PlanOutcomeRow> = (0..12)
            .map(|i| PlanOutcomeRow {
                id: format!("t{i}"),
                agent: "Explore".into(),
                status: "completed".into(),
                response: "r".repeat(2000),
            })
            .collect();
        let prompt = build_synthesis_prompt("do the thing", &rows);
        for i in 0..12 {
            assert!(prompt.contains(&format!("id=t{i}")), "task {i} dropped");
        }
        let payload: usize = prompt
            .lines()
            .skip_while(|l| !l.starts_with("- id="))
            .map(|l| l.len())
            .sum();
        assert!(
            payload <= SYNTHESIS_MAX_CHARS + rows.len() * SYNTHESIS_LINE_OVERHEAD,
            "total bounded: {payload}"
        );
        // Narrow plans keep the full per-task excerpt cap.
        assert_eq!(per_task_excerpt_budget(1), SYNTHESIS_EXCERPT_CHARS);
        assert_eq!(per_task_excerpt_budget(0), SYNTHESIS_EXCERPT_CHARS);
    }

    // SC-012 acceptance: the block is deterministic, never omitted, and
    // carries statuses + verdict + archive + synthesis honesty.
    #[test]
    fn block_renders_rows_verdict_archive_synthesis() {
        let snap = PlanOutcomeSnapshot {
            plan_id: "p1".into(),
            rows: vec![
                PlanOutcomeRow {
                    id: "a".into(),
                    agent: "Explore".into(),
                    status: "completed".into(),
                    response: "done\nthing".into(),
                },
                PlanOutcomeRow {
                    id: "b".into(),
                    agent: "Librarian".into(),
                    status: "skipped".into(),
                    response: "upstream failed".into(),
                },
            ],
            plan_approved: true,
            delivery_ready: false,
            archived: Some("p.md".into()),
            triage: Vec::new(),
        };
        // W8.2: the default rendering is the TUI markdown table.
        let block = render_plan_result_block(&snap, "ok");
        assert!(
            block.contains("| a | Explore | completed | done thing |\n"),
            "{block}"
        );
        assert!(
            block.contains("| b | Librarian | skipped | upstream failed |\n"),
            "{block}"
        );
        assert!(
            block.contains("verdict: plan_approved=true delivery_ready=false\n"),
            "{block}"
        );
        assert!(block.contains("archived: p.md\n"), "{block}");
        assert!(block.contains("synthesis: ok\n"), "{block}");
        // No archive ⇒ no archived line (byte-honest block).
        let mut snap_no_arch = snap.clone();
        snap_no_arch.archived = None;
        let block2 = render_plan_result_block(&snap_no_arch, "skipped:sufficient");
        assert!(!block2.contains("archived:"), "{block2}");
        assert!(
            block2.contains("synthesis: skipped:sufficient\n"),
            "{block2}"
        );
    }

    // W8.2: one builder, two renderings — every task in slot order on
    // both (failed/skipped included); plain rendering has no raw pipes
    // and maps completed→ok; both are pure functions of their inputs.
    #[test]
    fn block_renderings_carry_all_tasks_and_plain_has_no_pipes() {
        let snap = PlanOutcomeSnapshot {
            plan_id: "p1".into(),
            rows: vec![
                PlanOutcomeRow {
                    id: "t1".into(),
                    agent: "Explore".into(),
                    status: "completed".into(),
                    response: "sweep | docs".into(),
                },
                PlanOutcomeRow {
                    id: "t2".into(),
                    agent: "Atlas".into(),
                    status: "failed".into(),
                    response: "boom".into(),
                },
                PlanOutcomeRow {
                    id: "t3".into(),
                    agent: "Librarian".into(),
                    status: "skipped".into(),
                    response: "upstream".into(),
                },
            ],
            plan_approved: false,
            delivery_ready: false,
            archived: None,
            triage: Vec::new(),
        };
        use zen_core::types::PlanResultFormat;
        let table = render_plan_result_block_in(PlanResultFormat::MarkdownTable, &snap, "ok");
        assert!(
            table.contains("| t1 | Explore | completed | sweep \\| docs |\n"),
            "{table}"
        );
        assert!(
            table.contains("| t2 | Atlas | failed | boom |\n"),
            "{table}"
        );
        assert!(
            table.contains("| t3 | Librarian | skipped | upstream |\n"),
            "{table}"
        );
        let plain = render_plan_result_block_in(PlanResultFormat::PlainText, &snap, "ok");
        assert!(
            plain.contains("- [ok] t1 · Explore · sweep / docs\n"),
            "completed maps to [ok], pipes sanitized: {plain}"
        );
        assert!(plain.contains("- [failed] t2 · Atlas · boom\n"), "{plain}");
        assert!(
            plain.contains("- [skipped] t3 · Librarian · upstream\n"),
            "{plain}"
        );
        assert!(
            !plain.lines().any(|l| l.contains('|')),
            "no raw pipes on text surfaces: {plain}"
        );
        assert_eq!(
            plain,
            render_plan_result_block_in(PlanResultFormat::PlainText, &snap, "ok")
        );
        assert_eq!(
            table,
            render_plan_result_block_in(PlanResultFormat::MarkdownTable, &snap, "ok")
        );
    }

    #[test]
    fn is_local_provider_covers_ollama_and_mock() {
        assert!(is_local_provider(&zen_provider::Provider::Ollama));
        assert!(is_local_provider(&zen_provider::Provider::Mock));
        assert!(is_local_provider(&zen_provider::Provider::Unknown(
            "mock".into()
        )));
        assert!(is_local_provider(&zen_provider::Provider::Unknown(
            "ollama".into()
        )));
        assert!(!is_local_provider(&zen_provider::Provider::OpenAI));
        assert!(!is_local_provider(&zen_provider::Provider::Anthropic));
        assert!(!is_local_provider(&zen_provider::Provider::Unknown(
            "openai".into()
        )));
        assert_eq!(
            provider_name(&zen_provider::Provider::Unknown("mock".into())),
            "mock"
        );
        assert_eq!(provider_name(&zen_provider::Provider::Ollama), "Ollama");
    }
}
