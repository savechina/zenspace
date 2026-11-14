//! Proactive heartbeat: 9am morning brief via qqbot push (FR-038 / D04).
//!
//! The brief is 3 wisdom lines (decisions / people+preferences / watch-outs
//! incl. ignored topics) plus an optional 4th W9.2 plan line
//! (`plans: N done overnight · M needs your call · K running`, emitted iff
//! the 24h `workflow_plans` window is non-empty), capped at
//! [`BRIEF_MAX_CHARS`]. Delivery is an outbox handoff:
//! the worker writes `logs/outbox/morning-brief-<date>.json` (recipient-agnostic
//! `{date, lines, chat_hint, sensitivity}`), which the gateway qqbot carrier
//! drains with an active send (`msg_id=None`). zen-agents must not depend on
//! zen-gateway, so the worker never touches `QqBotApi` directly — the outbox
//! file is the seam.
//!
//! Sensitivity (outbox contract): every payload carries a `sensitivity` field
//! (`"Public" | "Private"` — [`Sensitivity`] Display strings; this worker never
//! stages `Confidential`), derived from the brief content by [`classify_brief`].
//! Consumer-side gate [`consumer_may_deliver`]: consumers not explicitly
//! allowlisted to carry personal content must skip non-`Public` briefs
//! (leave the file in place, never delete) so `Private` briefs stay local-only.
//! The nightly 3AM distillation leg is covered by `DreamWorker` (`0 0 2 * * *`);
//! this worker only covers the 9am push face.

use anyhow::Result;
use chrono::Timelike;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_repo::{WorkflowPlanRow, WorkflowRepo};
use zen_vault::SharedSqliteClient;
use zen_vault::distill::reflection::plan_archive_slug;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Cron: 9am daily (6-field, seconds first — same family as DreamWorker).
pub const MORNING_BRIEF_SCHEDULE: &str = "0 0 9 * * *";

/// Hard cap on the rendered brief body.
pub const BRIEF_MAX_CHARS: usize = 300;

/// W9.2 plan-line window: `workflow_plans` rows touched in the last 24h.
const PLAN_WINDOW_SECS: i64 = 24 * 3600;

/// Outbox file the qqbot carrier drains (push, not CLI pull).
pub fn outbox_path(paths: &ZenPaths, date: &str) -> std::path::PathBuf {
    paths
        .logs()
        .join("outbox")
        .join(format!("morning-brief-{date}.json"))
}

pub struct MorningBriefWorker {
    scheduled: Option<&'static str>,
}

impl MorningBriefWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for MorningBriefWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Placeholder bodies — also the `Public`-classification sentinel set
/// (see [`classify_brief`]).
const P_NO_DECISIONS: &str = "no recent decisions";
const P_NO_PEOPLE: &str = "no one new";
const P_NO_WATCHOUTS: &str = "all clear";

/// One rendered line of the brief.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefLine {
    pub label: &'static str,
    pub body: String,
}

impl BriefLine {
    fn render(&self) -> String {
        format!("{}: {}", self.label, self.body)
    }
}

/// Compose the 3-line brief from on-disk wisdom surfaces. Every source is
/// optional — absence yields an explicit placeholder, never an error.
pub fn compose_brief(paths: &ZenPaths) -> Vec<BriefLine> {
    let wisdom = paths.wiki().join("wisdom");
    let decisions = newest_stems(&wisdom.join("decisions"), 3);
    let decisions = if decisions.is_empty() {
        newest_stems(&wisdom.join("beliefs"), 3)
    } else {
        decisions
    };
    let people = newest_preference_subjects(&wisdom.join("preferences"), 3);
    let mut watchouts = newest_stems(&wisdom.join("anti-patterns"), 2);
    let overdue = count_files(&paths.memory().join("commitments"));
    if overdue > 0 {
        watchouts.push(format!("{overdue} open commitments"));
    }
    let ignored = ignored_topics(paths);
    if !ignored.is_empty() {
        watchouts.push(format!("muted: {}", ignored.join(", ")));
    }
    vec![
        BriefLine {
            label: "Decisions",
            body: or_placeholder(decisions.join(" · "), P_NO_DECISIONS),
        },
        BriefLine {
            label: "People",
            body: or_placeholder(people.join(" · "), P_NO_PEOPLE),
        },
        BriefLine {
            label: "Watch-outs",
            body: or_placeholder(watchouts.join(" · "), P_NO_WATCHOUTS),
        },
    ]
}

/// Content-derived classification ([`Sensitivity`] taxonomy): a brief whose
/// every line is a placeholder carries no personal knowledge → `Public`; any
/// real wisdom surface (decisions, beliefs, preferences, commitments, muted
/// topics) is personal → `Private` (local-only unless the consumer is
/// allowlisted — see [`consumer_may_deliver`]). Never returns `Confidential`.
fn classify_brief(lines: &[BriefLine]) -> Sensitivity {
    let all_placeholders = lines.iter().all(|l| {
        matches!(
            l.body.as_str(),
            P_NO_DECISIONS | P_NO_PEOPLE | P_NO_WATCHOUTS
        )
    });
    if all_placeholders {
        Sensitivity::Public
    } else {
        Sensitivity::Private
    }
}

/// Consumer-side gate for outbox briefs (outbox contract; enforced by the
/// gateway qqbot drainer, see TODOS.md "QQBot outbox drainer").
///
/// Functionality: allowlisted consumers (surfaces explicitly permitted to
/// carry personal content) may deliver any staged brief; every other consumer
/// may only deliver `Public` briefs and must skip the rest — leave the file
/// in place (never delete) so an allowlisted drainer can pick it up later.
/// User impact: `Private` briefs stay local-only unless the surface opted in.
/// Default: consumers are NOT allowlisted (fail-closed).
/// Interaction: complements the qqbot chat allowlist (`[channels.qqbot]`
/// `allowed_users`) — chat allowlisting gates who talks; this gates what the
/// drainer may push.
pub fn consumer_may_deliver(sensitivity: Sensitivity, consumer_allowlisted: bool) -> bool {
    consumer_allowlisted || sensitivity == Sensitivity::Public
}

/// Render lines into the capped body (3 wisdom lines + the optional W9.2
/// plan line).
pub fn render_brief(lines: &[BriefLine]) -> String {
    let mut body = lines
        .iter()
        .take(4)
        .map(BriefLine::render)
        .collect::<Vec<_>>()
        .join("\n");
    if body.chars().count() > BRIEF_MAX_CHARS {
        body = body.chars().take(BRIEF_MAX_CHARS - 1).collect::<String>() + "…";
    }
    body
}

fn or_placeholder(s: String, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// W9.2 overnight plan tallies. Pure (no DB): `done` is gated on the archived
/// page existing on disk (the F5 named reader — a completed row without its
/// `wisdom/plans/<slug>.md` page is NOT counted done), `needs_call` covers
/// failures plus stale-owner `running` rows, `running` the fresh ones.
/// `cancelled` rows count nowhere.
#[derive(Debug, Default, PartialEq, Eq)]
struct PlanCounts {
    done: usize,
    needs_call: usize,
    running: usize,
}

fn count_plans(rows: &[WorkflowPlanRow], plans_dir: &Path, now: i64) -> PlanCounts {
    let mut counts = PlanCounts::default();
    // W11.3/T3: one live staleness definition (resolved once per call, not
    // per row) — the brief's "needs a call" classification matches the
    // sweep and the resume fence under a widened watchdog.
    let stale_secs = zen_core::config::resolve_stale_claim_secs();
    for row in rows {
        match row.status.as_str() {
            "completed" => {
                let slug = plan_archive_slug(row.name.as_deref(), None, &row.plan_id);
                if plans_dir.join(format!("{slug}.md")).exists() {
                    counts.done += 1;
                }
            }
            "failed" => counts.needs_call += 1,
            // A `running` row whose claim went stale (no progress for the
            // live staleness window — W11.3/T3 unified definition) is no
            // longer progressing: it needs a human call.
            "running" if row.updated_at < now - stale_secs => {
                counts.needs_call += 1;
            }
            "running" => counts.running += 1,
            _ => {}
        }
    }
    counts
}

/// W9.2 plan line — `Some` iff the 24h window holds at least one row (zero
/// rows ⇒ `None` ⇒ no line). Body format is spec-frozen:
/// `{N} done overnight · {M} needs your call · {K} running` (all three
/// segments always present).
fn plan_line(rows: &[WorkflowPlanRow], plans_dir: &Path, now: i64) -> Option<BriefLine> {
    if rows.is_empty() {
        return None;
    }
    let c = count_plans(rows, plans_dir, now);
    Some(BriefLine {
        label: "plans",
        body: format!(
            "{} done overnight · {} needs your call · {} running",
            c.done, c.needs_call, c.running
        ),
    })
}

/// W9.2 window fetch: `workflow_plans` rows touched at/after `cutoff`,
/// newest first. Fail-open by contract — a missing `state.db` (cold or test
/// home; the brief worker must never CREATE the daemon's database as a
/// side effect) or any open/query error yields `None` with a `warn!`; the
/// brief still succeeds, just without the plan line.
async fn recent_plan_rows(db_path: &Path, cutoff: i64) -> Option<Vec<WorkflowPlanRow>> {
    if !db_path.exists() {
        return None;
    }
    let shared = SharedSqliteClient::new(db_path.to_path_buf());
    let client = match shared.get().await {
        Ok(client) => client,
        Err(e) => {
            warn!(error = %e, "morning-brief plan stats: state.db unavailable; plan line omitted");
            return None;
        }
    };
    match WorkflowRepo::new(&client).recent_plans(cutoff).await {
        Ok(rows) => Some(rows),
        Err(e) => {
            warn!(error = %e, "morning-brief plan stats: query failed; plan line omitted");
            None
        }
    }
}

fn newest_stems(dir: &Path, n: usize) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    files.sort_by_key(|e| {
        e.metadata()
            .and_then(|m| m.modified())
            .map(std::cmp::Reverse)
            .unwrap_or(std::cmp::Reverse(std::time::SystemTime::UNIX_EPOCH))
    });
    files
        .into_iter()
        .take(n)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.replace(['-', '_'], " "))
        })
        .collect()
}

fn newest_preference_subjects(dir: &Path, n: usize) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut subjects = Vec::new();
    for entry in entries.filter_map(|e| e.ok()).take(n * 3) {
        if subjects.len() >= n {
            break;
        }
        let Ok(content) = fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in content.lines().take(12) {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("subject:") {
                let s = rest.trim().trim_matches('"').to_string();
                if !s.is_empty() && !subjects.contains(&s) {
                    subjects.push(s);
                }
                break;
            }
        }
    }
    subjects
}

fn count_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .count()
        })
        .unwrap_or(0)
}

/// Topics the user muted — `logs/ignored-topics.json` (`{"topics": [...]}`).
/// Absence means nothing muted (not an error).
fn ignored_topics(paths: &ZenPaths) -> Vec<String> {
    let path = paths.logs().join("ignored-topics.json");
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v.get("topics").cloned())
        .and_then(|t| serde_json::from_value::<Vec<String>>(t).ok())
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl ZenWorker for MorningBriefWorker {
    fn id(&self) -> &'static str {
        "morning-brief"
    }

    fn description(&self) -> &'static str {
        "Proactive heartbeat: 9am 3-line brief staged to the qqbot outbox"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or(MORNING_BRIEF_SCHEDULE)
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let paths = ZenPaths::detect()?;
        self.execute_with_paths(&paths, ctx).await
    }
}

impl MorningBriefWorker {
    async fn execute_with_paths(
        &self,
        paths: &ZenPaths,
        ctx: &WorkerContext,
    ) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let date = ctx.now.format("%Y-%m-%d").to_string();

        let mut lines = compose_brief(paths);
        // W9.2 plan line — appended last (after the wisdom lines). Fail-open:
        // a missing/unreadable state.db contributes no line and never fails
        // the worker.
        let now_ts = ctx.now.timestamp();
        if let Some(rows) =
            recent_plan_rows(&paths.data().join("state.db"), now_ts - PLAN_WINDOW_SECS).await
            && let Some(line) = plan_line(&rows, &paths.wiki().join("wisdom").join("plans"), now_ts)
        {
            lines.push(line);
        }
        let outbox = outbox_path(paths, &date);
        if let Some(parent) = outbox.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            warn!(error = %e, "morning-brief outbox mkdir failed; brief logged only");
        }
        let sensitivity = classify_brief(&lines);
        let payload = serde_json::json!({
            "date": date,
            "lines": lines.iter().map(BriefLine::render).collect::<Vec<_>>(),
            "chat_hint": "qqbot active send (msg_id=None)",
            "sensitivity": sensitivity.to_string(),
        });
        match fs::write(
            &outbox,
            serde_json::to_string_pretty(&payload).unwrap_or_default(),
        ) {
            Ok(()) => {
                info!(outbox = %outbox.display(), hour = ctx.now.hour(), sensitivity = %sensitivity, "morning brief staged for qqbot push")
            }
            Err(e) => warn!(error = %e, "morning-brief outbox write failed; brief logged only"),
        }
        // CSO audit: the brief body is journal/memory-derived — it must not
        // flow into zen.log. It is staged in the outbox above for inspection;
        // here we log only the location and the already-computed sensitivity.
        info!(outbox = %outbox.display(), hour = ctx.now.hour(), sensitivity = %sensitivity, "morning brief body staged in outbox");

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: lines.len(),
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ZenPaths) {
        let dir = TempDir::new().unwrap();
        let paths = ZenPaths::for_testing(dir.path().to_path_buf());
        (dir, paths)
    }

    #[test]
    fn brief_is_three_lines_and_capped() {
        let (_dir, paths) = setup();
        let lines = compose_brief(&paths);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].label, "Decisions");
        assert_eq!(lines[1].label, "People");
        assert_eq!(lines[2].label, "Watch-outs");
        let body = render_brief(&lines);
        assert!(body.chars().count() <= BRIEF_MAX_CHARS);
        assert!(body.contains("no recent decisions"));
    }

    #[test]
    fn brief_picks_up_wisdom_surfaces_and_ignored_topics() {
        let (_dir, paths) = setup();
        let wisdom = paths.wiki().join("wisdom");
        fs::create_dir_all(wisdom.join("decisions")).unwrap();
        fs::write(wisdom.join("decisions/all-in-bet.md"), "---\n---\n").unwrap();
        fs::create_dir_all(wisdom.join("preferences")).unwrap();
        fs::write(
            wisdom.join("preferences/p1.md"),
            "---\nsubject: Ada\npredicate: likes\nobject: terse code\n---\n",
        )
        .unwrap();
        fs::create_dir_all(paths.logs()).unwrap();
        fs::write(
            paths.logs().join("ignored-topics.json"),
            r#"{"topics": ["crypto"]}"#,
        )
        .unwrap();
        let lines = compose_brief(&paths);
        assert!(
            lines[0].body.contains("all in bet"),
            "got: {}",
            lines[0].body
        );
        assert!(lines[1].body.contains("Ada"), "got: {}", lines[1].body);
        assert!(lines[2].body.contains("crypto"), "got: {}", lines[2].body);
    }

    #[test]
    fn worker_identity_and_schedule() {
        let worker = MorningBriefWorker::new();
        assert_eq!(worker.id(), "morning-brief");
        assert_eq!(worker.schedule(), MORNING_BRIEF_SCHEDULE);
    }

    #[test]
    fn placeholder_only_brief_classifies_public() {
        let (_dir, paths) = setup();
        let lines = compose_brief(&paths);
        assert_eq!(classify_brief(&lines), Sensitivity::Public);
    }

    #[tokio::test]
    async fn execute_stages_outbox_with_sensitivity_field() {
        let (_dir, paths) = setup();
        let wisdom = paths.wiki().join("wisdom");
        fs::create_dir_all(wisdom.join("decisions")).unwrap();
        fs::write(wisdom.join("decisions/private-bet.md"), "---\n---\n").unwrap();

        let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, 4, 9, 0, 0).unwrap();
        let worker = MorningBriefWorker::new();
        let report = worker
            .execute_with_paths(&paths, &WorkerContext::new(now))
            .await
            .unwrap();
        assert!(report.success);
        assert_eq!(report.fact_count, 3);

        let date = now.format("%Y-%m-%d").to_string();
        let outbox = outbox_path(&paths, &date);
        assert!(outbox.exists(), "outbox staged at {}", outbox.display());
        let payload: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&outbox).unwrap()).unwrap();
        let lines = payload["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0]
                .as_str()
                .unwrap()
                .starts_with("Decisions: private bet")
        );
        assert_eq!(payload["sensitivity"], "Private");

        assert!(!consumer_may_deliver(Sensitivity::Private, false));
        assert!(consumer_may_deliver(Sensitivity::Private, true));
        assert!(consumer_may_deliver(Sensitivity::Public, false));
    }

    // W9.2 plan line pins.

    fn plan_row(
        plan_id: &str,
        name: Option<&str>,
        status: &str,
        updated_at: i64,
    ) -> WorkflowPlanRow {
        WorkflowPlanRow {
            plan_id: plan_id.to_string(),
            name: name.map(|s| s.to_string()),
            status: status.to_string(),
            spec_json: "{}".to_string(),
            summary: None,
            plan_approved: None,
            delivery_ready: None,
            owner: None,
            created_at: updated_at - 60,
            updated_at,
        }
    }

    fn write_archive_page(plans_dir: &Path, plan_name: Option<&str>, plan_id: &str) {
        let tasks = [zen_vault::distill::reflection::PlanArchiveTask {
            id: "a",
            agent: "Explore",
            status: "completed",
            response: "ok",
        }];
        let archive = zen_vault::distill::reflection::PlanArchive {
            plan_name,
            plan_id,
            tasks: &tasks,
            plan_approved: true,
            delivery_ready: true,
            duration_ms: 1,
        };
        zen_vault::distill::reflection::record_plan_archive(plans_dir, &archive).unwrap();
    }

    #[test]
    fn zero_plans_emits_no_line() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        assert_eq!(plan_line(&[], &plans_dir, 10_000), None);
    }

    #[test]
    fn completed_plan_with_page_counts_done() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        write_archive_page(&plans_dir, Some("Overnight Sweep"), "p-done");
        let rows = [plan_row(
            "p-done",
            Some("Overnight Sweep"),
            "completed",
            9_000,
        )];
        let counts = count_plans(&rows, &plans_dir, 10_000);
        assert_eq!(
            counts,
            PlanCounts {
                done: 1,
                needs_call: 0,
                running: 0
            }
        );
    }

    #[test]
    fn completed_plan_without_page_not_done() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        let rows = [plan_row(
            "p-ghost",
            Some("Never Archived"),
            "completed",
            9_000,
        )];
        assert_eq!(
            count_plans(&rows, &plans_dir, 10_000),
            PlanCounts::default(),
            "F5 named reader: a completed row without its archived page counts nowhere"
        );
    }

    #[test]
    fn stale_owner_running_counts_as_needs_call() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        let now = 10_000;
        let rows = [
            // Claim stale: no progress for the live staleness window
            // (W11.3/T3 — the same resolver count_plans uses).
            plan_row(
                "p-stale",
                None,
                "running",
                now - zen_core::config::resolve_stale_claim_secs() - 1,
            ),
            // Fresh claim: still progressing.
            plan_row("p-live", None, "running", now - 60),
        ];
        let counts = count_plans(&rows, &plans_dir, now);
        assert_eq!(counts.needs_call, 1, "stale-owner running needs a call");
        assert_eq!(counts.running, 1, "fresh running stays running");
        assert_eq!(counts.done, 0);
    }

    #[test]
    fn failed_counts_needs_call_and_cancelled_counts_nowhere() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        let rows = [
            plan_row("p-fail", None, "failed", 9_000),
            plan_row("p-cancel", None, "cancelled", 9_000),
        ];
        assert_eq!(
            count_plans(&rows, &plans_dir, 10_000),
            PlanCounts {
                done: 0,
                needs_call: 1,
                running: 0
            }
        );
    }

    #[test]
    fn plan_line_exact_format() {
        let (_dir, paths) = setup();
        let plans_dir = paths.wiki().join("wisdom").join("plans");
        write_archive_page(&plans_dir, Some("Good One"), "p-ok");
        let now = 10_000;
        let rows = [
            plan_row("p-ok", Some("Good One"), "completed", 9_000),
            plan_row("p-bad", None, "failed", 9_000),
            plan_row("p-live", None, "running", now - 60),
        ];
        let line = plan_line(&rows, &plans_dir, now).unwrap();
        assert_eq!(line.label, "plans");
        assert_eq!(
            line.body,
            "1 done overnight · 1 needs your call · 1 running"
        );
        assert_eq!(
            line.render(),
            "plans: 1 done overnight · 1 needs your call · 1 running"
        );
    }

    #[tokio::test]
    async fn missing_state_db_emits_no_plan_line_and_brief_still_succeeds() {
        let (_dir, paths) = setup();
        assert!(!paths.data().join("state.db").exists());
        let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, 4, 9, 0, 0).unwrap();
        let worker = MorningBriefWorker::new();
        let report = worker
            .execute_with_paths(&paths, &WorkerContext::new(now))
            .await
            .unwrap();
        assert!(report.success);
        let payload: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(outbox_path(&paths, "2026-09-04")).unwrap())
                .unwrap();
        let lines = payload["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3, "no plan line without state.db");
        assert!(!payload.to_string().contains("plans: "));
    }

    #[tokio::test]
    async fn execute_stages_plan_line_from_state_db() {
        let (_dir, paths) = setup();
        let now = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, 4, 9, 0, 0).unwrap();
        let now_ts = now.timestamp();

        let db_path = paths.data().join("state.db");
        let client = zen_repo::SqliteClient::open(&db_path).await.unwrap();
        let repo = zen_repo::WorkflowRepo::new(&client);
        repo.create_plan("p1", Some("Overnight Sweep"), "{}", now_ts - 200)
            .await
            .unwrap();
        repo.complete_plan(
            "p1",
            "completed",
            Some("done"),
            Some(true),
            Some(true),
            now_ts - 100,
        )
        .await
        .unwrap();
        drop(client);

        let plans_dir = paths.wiki().join("wisdom").join("plans");
        write_archive_page(&plans_dir, Some("Overnight Sweep"), "p1");

        let worker = MorningBriefWorker::new();
        let report = worker
            .execute_with_paths(&paths, &WorkerContext::new(now))
            .await
            .unwrap();
        assert!(report.success);
        assert_eq!(report.fact_count, 4);

        let payload: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(outbox_path(&paths, "2026-09-04")).unwrap())
                .unwrap();
        let lines = payload["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 4, "wisdom trio + plan line");
        assert_eq!(
            lines[3].as_str().unwrap(),
            "plans: 1 done overnight · 0 needs your call · 0 running"
        );
        // Real workflow history is personal: the plan line is not a
        // placeholder sentinel, so the brief flips to Private.
        assert_eq!(payload["sensitivity"], "Private");
    }
}
