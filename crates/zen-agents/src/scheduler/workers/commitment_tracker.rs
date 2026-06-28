use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_memory::commitment::Commitment;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

#[derive(Debug, Clone)]
pub struct AntiTalkIndicator {
    pub commitment_slug: String,
    pub mention_count: usize,
    pub milestone_count: usize,
    pub ratio: f64,
    pub is_warning: bool,
}

impl AntiTalkIndicator {
    pub fn display_summary(&self) -> String {
        if self.is_warning {
            format!(
                "空谈警报: {} (mentions={}, milestones={}, ratio={:.1})",
                self.commitment_slug, self.mention_count, self.milestone_count, self.ratio
            )
        } else {
            format!(
                "{}: mentions={}, milestones={}, ratio={:.1}",
                self.commitment_slug, self.mention_count, self.milestone_count, self.ratio
            )
        }
    }
}

pub struct CommitmentTracker {
    scheduled: Option<&'static str>,
}

impl CommitmentTracker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for CommitmentTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for CommitmentTracker {
    fn id(&self) -> &'static str {
        "commitment-tracker"
    }

    fn description(&self) -> &'static str {
        "Track commitments from journal entries, manage lifecycle and due-date review"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 8 * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let journal_dir = paths.journal_entries();
        if !journal_dir.is_dir() {
            debug!("journal entries directory does not exist, skipping commitment tracking");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let commitments_dir = paths.vault().join("memories/commitments");
        fs::create_dir_all(&commitments_dir).with_context(|| {
            format!(
                "failed to create commitments dir: {}",
                commitments_dir.display()
            )
        })?;

        let mut total_tracked = 0usize;
        let mut due_count = 0usize;
        let mut slugs_used: Vec<String> = Vec::new();

        for entry in fs::read_dir(&journal_dir)
            .with_context(|| format!("failed to read journal dir: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }

            if !is_journaled(&path) {
                continue;
            }
            if JournalEntryState::has_commitment_tracked(&path) {
                continue;
            }

            match extract_commitments_from_journal(&path) {
                Ok(items) if !items.is_empty() => {
                    for item in &items {
                        let slug = unique_slug(&item.text, &slugs_used);
                        slugs_used.push(slug.clone());

                        let commitment_path = commitments_dir.join(format!("{slug}.md"));
                        if commitment_path.exists() {
                            continue;
                        }

                        let mut commitment = Commitment::from_raw(&item.text);
                        commitment.review_at =
                            Some((item.created_at + chrono::Duration::days(7)).date_naive());
                        commitment.source_journal = Some(item.session_id.clone());
                        if let Err(e) = commitment.save(&commitments_dir) {
                            warn!(
                                text = %item.text,
                                error = %e,
                                "failed to save commitment"
                            );
                            continue;
                        }
                        total_tracked += 1;

                        let base = base_slug_of(&slug);
                        let prior_count = count_similar_commitments(&commitments_dir, &base);
                        if prior_count >= 3 {
                            warn!(
                                commitment = %item.text,
                                base_slug = %base,
                                occurrences = prior_count,
                                "anti-talk pattern detected: commitment repeatedly created without closure"
                            );
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to extract commitments from journal entry");
                }
            }

            let now = Utc::now();
            let state = JournalEntryState {
                commitment_tracked_at: Some(now.to_rfc3339()),
                ..Default::default()
            };
            if let Err(e) = state.save(&path) {
                warn!(path = %path.display(), error = %e, "failed to mark journal entry as commitment-tracked");
            }
        }

        let commitments = Commitment::load_all(&commitments_dir).unwrap_or_default();
        let mut stop_loss_count = 0usize;
        let mut anti_talk_warnings = 0usize;
        for c in &commitments {
            if c.is_overdue() {
                warn!(commitment = %c.what, "commitment review due");
                due_count += 1;
            }
            if c.stop_loss.triggered {
                warn!(
                    commitment = %c.what,
                    state = %c.state,
                    "commitment has triggered stop-loss"
                );
                stop_loss_count += 1;
            }
        }

        let journal_entries_dir = paths.journal_entries();
        let anti_talks = compute_all_anti_talk(&commitments, &journal_entries_dir);
        for at in &anti_talks {
            if at.is_warning {
                warn!(
                    slug = %at.commitment_slug,
                    mentions = at.mention_count,
                    milestones = at.milestone_count,
                    ratio = at.ratio,
                    "anti-talk warning: talk-to-achievement ratio exceeds threshold"
                );
                anti_talk_warnings += 1;
            }
        }

        if due_count > 0 {
            info!(due_count, "commitments due for review");
        }
        if stop_loss_count > 0 {
            info!(
                stop_loss_count,
                "commitments with triggered stop-loss require attention"
            );
        }
        if anti_talk_warnings > 0 {
            info!(
                anti_talk_warnings,
                "commitments with excessive talk-to-achievement ratio detected"
            );
        }

        if total_tracked > 0 {
            info!(
                tracked = total_tracked,
                "commitments extracted from journal entries"
            );
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_tracked,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct CommitmentItem {
    text: String,
    session_id: String,
    created_at: DateTime<Utc>,
}

fn extract_commitments_from_journal(path: &Path) -> Result<Vec<CommitmentItem>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let mut items = Vec::new();
    let mut in_commitments = false;
    let mut session_id = "unknown".to_string();
    let mut date_str = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("session_id:") {
            if let Some(val) = trimmed.strip_prefix("session_id:") {
                session_id = val.trim().to_string();
            }
        } else if trimmed.starts_with("date:") {
            if let Some(val) = trimmed.strip_prefix("date:") {
                date_str = Some(val.trim().to_string());
            }
        } else if trimmed == "## Commitments" {
            in_commitments = true;
            continue;
        } else if trimmed.starts_with("## ") {
            in_commitments = false;
            continue;
        }

        if in_commitments && let Some(item) = trimmed.strip_prefix("- ") {
            let text = item.trim().to_string();
            if !text.is_empty() && !text.starts_with("_(no ") {
                let created_at = date_str
                    .as_ref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
                    .unwrap_or_else(Utc::now);
                items.push(CommitmentItem {
                    text,
                    session_id: session_id.clone(),
                    created_at,
                });
            }
        }
    }

    Ok(items)
}

fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .chars()
        .take(60)
        .collect()
}

/// Strip a trailing `-{N}` numeric suffix from a slug stem.
/// `write-tests` → `write-tests`
/// `write-tests-2` → `write-tests`
/// `write-tests-10` → `write-tests`
/// `实现登录功能` → `实现登录功能` (no ASCII digit suffix, unchanged)
fn base_slug_of(stem: &str) -> String {
    if let Some(idx) = stem.rfind('-') {
        let suffix = &stem[idx + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return stem[..idx].to_string();
        }
    }
    stem.to_string()
}

/// Count existing commitment files in `dir` whose stem shares the same `base_slug`
/// (ignoring `-N` numeric suffixes). Does NOT count the file currently being created.
fn count_similar_commitments(dir: &Path, base_slug: &str) -> usize {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if base_slug_of(stem) == base_slug {
            count += 1;
        }
    }
    count
}

fn unique_slug(text: &str, used: &[String]) -> String {
    let base = slugify(text);
    if !used.contains(&base) {
        return base;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{base}-{counter}");
        if !used.contains(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

fn is_journaled(path: &Path) -> bool {
    if JournalEntryState::is_journaled(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::is_journaled(path)
}

fn compute_anti_talk_indicator(
    commitment: &Commitment,
    journal_dir: &Path,
) -> Result<AntiTalkIndicator> {
    let slug = commitment.slug();
    let what_lower = commitment.what.to_lowercase();

    let mut mention_count = 0usize;
    if journal_dir.is_dir() {
        for entry in fs::read_dir(journal_dir)
            .with_context(|| format!("failed to read journal dir: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for line in content.lines() {
                if line.to_lowercase().contains(&what_lower) {
                    mention_count += 1;
                }
            }
        }
    }

    let milestone_count = commitment.milestones.iter().filter(|m| m.completed).count();

    let denominator = std::cmp::max(milestone_count, 1) as f64;
    let ratio = mention_count as f64 / denominator;
    let is_warning = ratio > 5.0;

    Ok(AntiTalkIndicator {
        commitment_slug: slug,
        mention_count,
        milestone_count,
        ratio,
        is_warning,
    })
}

fn compute_all_anti_talk(commitments: &[Commitment], journal_dir: &Path) -> Vec<AntiTalkIndicator> {
    let mut indicators: Vec<AntiTalkIndicator> = commitments
        .iter()
        .filter_map(|c| match compute_anti_talk_indicator(c, journal_dir) {
            Ok(indicator) => Some(indicator),
            Err(e) => {
                warn!(commitment = %c.what, error = %e, "failed to compute anti-talk indicator");
                None
            }
        })
        .collect();

    indicators.sort_by(|a, b| {
        b.ratio
            .partial_cmp(&a.ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    indicators
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_extract_commitments_from_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: 01JX001\ndate: 2026-06-26\n---\n\n# Session Journal\n\n## Commitments\n\n- simplify login by July\n- write integration tests this week\n";
        fs::write(&path, content).unwrap();

        let items = extract_commitments_from_journal(&path).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "simplify login by July");
        assert_eq!(items[0].session_id, "01JX001");
        assert_eq!(items[1].text, "write integration tests this week");
    }

    #[test]
    fn test_extract_commitments_empty_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content =
            "---\nsession_id: test\n---\n\n## Commitments\n\n_(no commitments extracted)_\n";
        fs::write(&path, content).unwrap();

        let items = extract_commitments_from_journal(&path).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_extract_commitments_no_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\n---\n\n## Facts\n\n- some fact\n";
        fs::write(&path, content).unwrap();

        let items = extract_commitments_from_journal(&path).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("test-case-123"), "test-case-123");
        assert_eq!(slugify("  trimmed  "), "trimmed");
    }

    #[test]
    fn test_slugify_truncates_long_text() {
        let long = "a".repeat(100);
        let result = slugify(&long);
        assert!(result.len() <= 60);
    }

    #[test]
    fn test_slugify_handles_unicode() {
        let result = slugify("实现登录功能");
        assert_eq!(result, "实现登录功能");
    }

    #[test]
    fn test_base_slug_of_no_suffix() {
        assert_eq!(base_slug_of("write-tests"), "write-tests");
        assert_eq!(base_slug_of("实现登录功能"), "实现登录功能");
    }

    #[test]
    fn test_base_slug_of_with_numeric_suffix() {
        assert_eq!(base_slug_of("write-tests-2"), "write-tests");
        assert_eq!(base_slug_of("write-tests-10"), "write-tests");
    }

    #[test]
    fn test_base_slug_of_suffix_only_digits_stripped() {
        assert_eq!(base_slug_of("fix-bug-123"), "fix-bug");
    }

    #[test]
    fn test_base_slug_of_trailing_dash_no_digits() {
        assert_eq!(base_slug_of("foo-"), "foo-");
    }

    #[test]
    fn test_count_similar_commitments_empty_dir() {
        let dir = tempdir().unwrap();
        assert_eq!(count_similar_commitments(dir.path(), "anything"), 0);
    }

    #[test]
    fn test_count_similar_commitments_matches_base() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("write-tests.md"), "").unwrap();
        fs::write(dir.path().join("write-tests-2.md"), "").unwrap();
        fs::write(dir.path().join("write-tests-3.md"), "").unwrap();
        fs::write(dir.path().join("other-thing.md"), "").unwrap();
        assert_eq!(count_similar_commitments(dir.path(), "write-tests"), 3);
    }

    #[test]
    fn test_count_similar_commitments_ignores_non_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("task.md"), "").unwrap();
        fs::write(dir.path().join("task.txt"), "").unwrap();
        fs::write(dir.path().join("task-2.md"), "").unwrap();
        assert_eq!(count_similar_commitments(dir.path(), "task"), 2);
    }

    #[test]
    fn test_compute_anti_talk_indicator_no_mentions() {
        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("memories/journal");
        fs::create_dir_all(&journal_dir).unwrap();
        fs::write(
            journal_dir.join("2026-06-01.md"),
            "# Journal\n\nNo relevant content.\n",
        )
        .unwrap();

        let mut commitment = Commitment::new("ship feature X");
        commitment.add_milestone("done", None);
        commitment.milestones[0].completed = true;

        let indicator = compute_anti_talk_indicator(&commitment, &journal_dir).unwrap();
        assert_eq!(indicator.mention_count, 0);
        assert_eq!(indicator.milestone_count, 1);
        assert!(!indicator.is_warning);
    }

    #[test]
    fn test_compute_anti_talk_indicator_warning() {
        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("memories/journal");
        fs::create_dir_all(&journal_dir).unwrap();

        let journal_content = "Talk about ship feature X a lot\n".repeat(10);
        fs::write(journal_dir.join("2026-06-01.md"), &journal_content).unwrap();

        let mut commitment = Commitment::new("ship feature X");
        commitment.add_milestone("done", None);

        let indicator = compute_anti_talk_indicator(&commitment, &journal_dir).unwrap();
        assert_eq!(indicator.mention_count, 10);
        assert_eq!(indicator.milestone_count, 0);
        assert!(indicator.is_warning);
        assert!(indicator.ratio > 5.0);
    }

    #[test]
    fn test_compute_anti_talk_indicator_no_warning_balanced() {
        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("memories/journal");
        fs::create_dir_all(&journal_dir).unwrap();

        let journal_content = "ship feature X mentioned here\n".repeat(3);
        fs::write(journal_dir.join("2026-06-01.md"), &journal_content).unwrap();

        let mut commitment = Commitment::new("ship feature X");
        for i in 0..3 {
            commitment.add_milestone(&format!("m{i}"), None);
            commitment.milestones[i].completed = true;
        }

        let indicator = compute_anti_talk_indicator(&commitment, &journal_dir).unwrap();
        assert_eq!(indicator.mention_count, 3);
        assert_eq!(indicator.milestone_count, 3);
        assert!(!indicator.is_warning);
    }

    #[test]
    fn test_compute_all_anti_talk_sorted_by_ratio_desc() {
        let dir = tempdir().unwrap();
        let journal_dir = dir.path().join("memories/journal");
        fs::create_dir_all(&journal_dir).unwrap();

        let mut c1 = Commitment::new("low talk");
        c1.add_milestone("m1", None);
        c1.milestones[0].completed = true;
        fs::write(journal_dir.join("2026-06-01.md"), "low talk mentioned\n").unwrap();

        let c2 = Commitment::new("high talk");
        let repeats = "high talk mentioned\n".repeat(20);
        fs::write(journal_dir.join("2026-06-02.md"), &repeats).unwrap();

        let indicators = compute_all_anti_talk(&[c1, c2], &journal_dir);
        assert_eq!(indicators.len(), 2);
        assert!(indicators[0].ratio >= indicators[1].ratio);
    }

    #[test]
    fn test_anti_talk_display_summary() {
        let warning = AntiTalkIndicator {
            commitment_slug: "ship-feature".into(),
            mention_count: 10,
            milestone_count: 1,
            ratio: 10.0,
            is_warning: true,
        };
        assert!(warning.display_summary().contains("空谈警报"));

        let ok = AntiTalkIndicator {
            commitment_slug: "other".into(),
            mention_count: 2,
            milestone_count: 3,
            ratio: 0.67,
            is_warning: false,
        };
        assert!(!ok.display_summary().contains("空谈警报"));
    }
}
