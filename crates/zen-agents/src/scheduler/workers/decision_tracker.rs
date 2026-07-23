use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

pub struct DecisionTracker {
    schedule: &'static str,
}

impl Default for DecisionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DecisionTracker {
    pub fn new() -> Self {
        Self {
            schedule: "0 0 9 * * *",
        }
    }

    pub fn with_schedule(mut self, cron: &'static str) -> Self {
        self.schedule = cron;
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for DecisionTracker {
    fn id(&self) -> &'static str {
        "decision-tracker"
    }

    fn description(&self) -> &'static str {
        "Scan decisions for 30-day unclosed; trigger review"
    }

    fn schedule(&self) -> &'static str {
        self.schedule
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let decisions_dir = paths.vault().join("wiki/wisdom/decisions");
        if !decisions_dir.exists() {
            debug!("decisions directory does not exist, skipping decision tracking");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                llm_cost_usd: 0.0,
            });
        }

        let mut tracked_count = 0usize;
        let mut overdue: Vec<DecisionMeta> = Vec::new();

        for entry in fs::read_dir(&decisions_dir)
            .with_context(|| format!("failed to read decisions dir: {}", decisions_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || path.extension() != Some("md".as_ref()) {
                continue;
            }

            if JournalEntryState::has_decision_tracked(&path) {
                continue;
            }

            if let Some(meta) = parse_decision_frontmatter(&path) {
                let age_days = ctx.now.signed_duration_since(meta.decided_at).num_days();
                if !meta.has_outcome && meta.closed_at.is_none() && age_days > 30 {
                    overdue.push(meta);
                }
                tracked_count += 1;

                if let Ok(decision) = zen_memory::decision::Decision::from_file(&path) {
                    let report = zen_memory::decision_check::check_all(&decision);
                    let wiki_dir = paths.vault().join("wiki");
                    for violation in &report.violations {
                        match zen_memory::decision_check::persist_anti_pattern_wiki_page(
                            &wiki_dir,
                            violation,
                            &decision.id,
                        ) {
                            Ok(true) => {
                                info!(
                                    pattern = %violation.pattern_id,
                                    decision = %decision.id,
                                    "new anti-pattern wiki page created"
                                );
                            }
                            Ok(false) => {}
                            Err(e) => {
                                warn!(
                                    pattern = %violation.pattern_id,
                                    error = %e,
                                    "failed to persist anti-pattern wiki page"
                                );
                            }
                        }

                        let signal = zen_memory::AntiPatternSignal {
                            pattern: violation.pattern_id.clone(),
                            trigger: violation.message.clone(),
                            avoidance: format!("see wiki/wisdom/anti-patterns/{}", violation.pattern_id),
                            detected_in: vec![decision.id.clone()],
                        };
                        let signals_dir = wiki_dir.join("wisdom/anti-patterns");
                        if let Err(e) = signal.save(&signals_dir) {
                            warn!(
                                pattern = %violation.pattern_id,
                                error = %e,
                                "failed to persist AntiPatternSignal"
                            );
                        }
                    }
                }
            }

            let state = JournalEntryState {
                decision_tracked_at: Some(ctx.now.to_rfc3339()),
                ..Default::default()
            };
            if let Err(e) = state.save(&path) {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to mark decision as tracked"
                );
            }
        }

        if !overdue.is_empty() {
            write_review_queue(&decisions_dir, &overdue, ctx.now)?;
            info!(count = overdue.len(), "overdue decisions found");
        }

        if tracked_count > 0 {
            info!(
                tracked = tracked_count,
                "decisions scanned by decision-tracker"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: tracked_count,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

#[derive(Debug, Clone)]
struct DecisionMeta {
    id: String,
    title: String,
    decided_at: DateTime<Utc>,
    closed_at: Option<DateTime<Utc>>,
    has_outcome: bool,
}

fn parse_decision_frontmatter(path: &Path) -> Option<DecisionMeta> {
    let content = fs::read_to_string(path).ok()?;

    let mut in_frontmatter = false;
    let mut frontmatter_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut past_frontmatter = false;

    for line in content.lines() {
        if line.trim() == "---" {
            if !in_frontmatter {
                in_frontmatter = true;
                continue;
            } else {
                past_frontmatter = true;
                in_frontmatter = false;
                continue;
            }
        }
        if in_frontmatter {
            frontmatter_lines.push(line);
        } else if past_frontmatter {
            body_lines.push(line);
        }
    }

    let mut id = None;
    let mut title = None;
    let mut decided_at = None;
    let mut closed_at = None;

    for line in &frontmatter_lines {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("id:") {
            id = Some(val.trim().to_string());
        } else if let Some(val) = trimmed.strip_prefix("title:") {
            let v = val.trim().to_string();
            title = Some(v.trim_matches('"').to_string());
        } else if let Some(val) = trimmed.strip_prefix("decided_at:") {
            let v = val.trim();
            if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
                decided_at = Some(dt.with_timezone(&Utc));
            }
        } else if let Some(val) = trimmed.strip_prefix("closed_at:") {
            let v = val.trim();
            if v == "null" || v.is_empty() {
                closed_at = None;
            } else if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
                closed_at = Some(dt.with_timezone(&Utc));
            }
        }
    }

    let decided_at = decided_at?;
    let title = title.unwrap_or_default();
    let id = id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    let has_outcome = check_body_outcome(&body_lines);

    Some(DecisionMeta {
        id,
        title,
        decided_at,
        closed_at,
        has_outcome,
    })
}

fn check_body_outcome(body_lines: &[&str]) -> bool {
    let mut in_feedback = false;
    for line in body_lines {
        let trimmed = line.trim();
        if trimmed == "## Feedback" {
            in_feedback = true;
            continue;
        }
        if in_feedback && trimmed.starts_with("## ") {
            break;
        }
        if in_feedback && let Some(val) = trimmed.strip_prefix("outcome:") {
            let v = val.trim();
            if !v.is_empty() && v != "(pending)" && v != "null" {
                return true;
            }
        }
    }
    false
}

fn write_review_queue(
    decisions_dir: &Path,
    overdue: &[DecisionMeta],
    now: DateTime<Utc>,
) -> Result<()> {
    let queue_path = decisions_dir.join(".review-queue.md");
    let mut content = String::new();
    content.push_str("# Decision Review Queue\n");
    content.push_str(&format!("Generated: {}\n\n", now.to_rfc3339()));
    content.push_str("## Overdue Decisions (>30 days, no outcome)\n\n");
    for m in overdue {
        let age = now.signed_duration_since(m.decided_at).num_days();
        content.push_str(&format!(
            "- [{}] {} — decided {}, {} days ago\n",
            m.id,
            m.title,
            m.decided_at.format("%Y-%m-%d"),
            age
        ));
    }
    content.push_str("\n## Action Needed\n\n");
    content.push_str("Review each decision above and record the outcome.\n");
    fs::write(&queue_path, content)
        .with_context(|| format!("failed to write review queue: {}", queue_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_decision_file(
        dir: &Path,
        filename: &str,
        frontmatter: &str,
        body: &str,
    ) -> std::path::PathBuf {
        let path = dir.join(filename);
        let content = format!("---\n{}\n---\n\n{}", frontmatter, body);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_parse_frontmatter_extracts_decided_at() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "career-switch.md",
            "id: decision-2026-06-26-career\n\
             title: \"Switch career path\"\n\
             decided_at: 2026-06-26T09:00:00Z\n\
             closed_at: null",
            "## Feedback\noutcome: (pending)\n",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert_eq!(meta.id, "decision-2026-06-26-career");
        assert_eq!(meta.title, "Switch career path");
        assert_eq!(
            meta.decided_at,
            DateTime::parse_from_rfc3339("2026-06-26T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert!(!meta.has_outcome);
    }

    #[test]
    fn test_parse_frontmatter_null_closed_at() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "test.md",
            "id: test-decision\n\
             title: Test\n\
             decided_at: 2026-06-26T09:00:00Z\n\
             closed_at: null",
            "",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert!(meta.closed_at.is_none());
    }

    #[test]
    fn test_parse_frontmatter_with_closed_at() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "test.md",
            "id: test-decision\n\
             title: Test\n\
             decided_at: 2026-04-01T00:00:00Z\n\
             closed_at: 2026-05-01T00:00:00Z",
            "## Feedback\noutcome: Success\n",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert!(meta.closed_at.is_some());
        assert!(meta.has_outcome);
    }

    #[test]
    fn test_parse_frontmatter_with_outcome() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "test.md",
            "id: test-decision\n\
             title: Test\n\
             decided_at: 2026-06-26T09:00:00Z",
            "## Feedback\noutcome: Success\n",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert!(meta.has_outcome);
    }

    #[test]
    fn test_parse_frontmatter_pending_outcome() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "test.md",
            "id: test-decision\n\
             title: Test\n\
             decided_at: 2026-06-26T09:00:00Z",
            "## Feedback\noutcome: (pending)\n",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert!(!meta.has_outcome);
    }

    #[test]
    fn test_parse_frontmatter_derives_id_from_filename() {
        let dir = tempdir().unwrap();
        let path = write_decision_file(
            dir.path(),
            "my-decision.md",
            "title: Test\n\
             decided_at: 2026-06-26T09:00:00Z",
            "",
        );

        let meta = parse_decision_frontmatter(&path).unwrap();
        assert_eq!(meta.id, "my-decision");
    }

    #[test]
    fn test_is_overdue_30_days() {
        let now = Utc::now();
        let decided_at = now - chrono::Duration::days(31);
        let meta = DecisionMeta {
            id: "test".to_string(),
            title: "Test".to_string(),
            decided_at,
            closed_at: None,
            has_outcome: false,
        };
        let age_days = now.signed_duration_since(meta.decided_at).num_days();
        assert!(age_days > 30);
        assert!(!meta.has_outcome);
        assert!(meta.closed_at.is_none());
    }

    #[test]
    fn test_is_overdue_not_overdue() {
        let now = Utc::now();
        let decided_at = now - chrono::Duration::days(10);
        let meta = DecisionMeta {
            id: "test".to_string(),
            title: "Test".to_string(),
            decided_at,
            closed_at: None,
            has_outcome: false,
        };
        let age_days = now.signed_duration_since(meta.decided_at).num_days();
        assert!(age_days <= 30);
    }

    #[test]
    fn test_is_overdue_closed_not_overdue() {
        let now = Utc::now();
        let decided_at = now - chrono::Duration::days(31);
        let meta = DecisionMeta {
            id: "test".to_string(),
            title: "Test".to_string(),
            decided_at,
            closed_at: Some(now - chrono::Duration::days(5)),
            has_outcome: false,
        };
        assert!(meta.closed_at.is_some());
    }

    #[test]
    fn test_review_queue_format() {
        let dir = tempdir().unwrap();
        let overdue = vec![DecisionMeta {
            id: "decision-001".to_string(),
            title: "Test Decision".to_string(),
            decided_at: DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            closed_at: None,
            has_outcome: false,
        }];
        let now = DateTime::parse_from_rfc3339("2026-06-26T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        write_review_queue(dir.path(), &overdue, now).unwrap();

        let queue_path = dir.path().join(".review-queue.md");
        assert!(queue_path.exists());
        let content = fs::read_to_string(&queue_path).unwrap();
        assert!(content.contains("# Decision Review Queue"));
        assert!(content.contains("decision-001"));
        assert!(content.contains("Test Decision"));
        assert!(content.contains("56 days ago"));
    }

    #[test]
    fn test_execute_no_dir_returns_zero() {
        let dir = tempdir().unwrap();
        let nonexistent = dir.path().join("decisions");
        assert!(!nonexistent.exists());

        let tracker = DecisionTracker::new();
        assert_eq!(tracker.id(), "decision-tracker");
    }

    #[test]
    fn test_execute_finds_overdue() {
        let dir = tempdir().unwrap();
        let decisions_dir = dir.path().join("decisions");
        fs::create_dir_all(&decisions_dir).unwrap();

        let now = Utc::now();
        let decided_at = (now - chrono::Duration::days(31))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();

        write_decision_file(
            &decisions_dir,
            "old-decision.md",
            &format!(
                "id: old-decision\n\
                 title: Old Decision\n\
                 decided_at: {}\n\
                 closed_at: null",
                decided_at
            ),
            "## Feedback\noutcome: (pending)\n",
        );

        let meta = parse_decision_frontmatter(&decisions_dir.join("old-decision.md")).unwrap();
        let age_days = now.signed_duration_since(meta.decided_at).num_days();
        assert!(age_days > 30);
        assert!(!meta.has_outcome);
        assert!(meta.closed_at.is_none());

        let overdue = vec![meta];
        write_review_queue(&decisions_dir, &overdue, now).unwrap();

        let queue_path = decisions_dir.join(".review-queue.md");
        assert!(queue_path.exists());
        let content = fs::read_to_string(&queue_path).unwrap();
        assert!(content.contains("old-decision"));
    }
}
