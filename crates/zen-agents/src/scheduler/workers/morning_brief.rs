//! Proactive heartbeat: 9am morning brief via qqbot push (FR-038 / D04).
//!
//! The brief is 3 lines (decisions / people+preferences / watch-outs incl.
//! ignored topics), capped at [`BRIEF_MAX_CHARS`]. Delivery is an outbox handoff:
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

use super::super::{WorkerContext, WorkerReport, ZenWorker};

/// Cron: 9am daily (6-field, seconds first — same family as DreamWorker).
pub const MORNING_BRIEF_SCHEDULE: &str = "0 0 9 * * *";

/// Hard cap on the rendered brief body.
pub const BRIEF_MAX_CHARS: usize = 300;

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

/// Render lines into the capped 3-line body.
pub fn render_brief(lines: &[BriefLine]) -> String {
    let mut body = lines
        .iter()
        .take(3)
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

        let lines = compose_brief(paths);
        let body = render_brief(&lines);
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
        info!("morning brief:\n{body}");

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
}
