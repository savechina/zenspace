use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use walkdir::WalkDir;

use crate::wiki::WikiPage;

/// A single knowledge gap identified by the learning loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeGap {
    /// Topic or title of the gap.
    pub topic: String,
    /// Confidence score (0.0 = low, 1.0 = high certainty this is a real gap).
    pub confidence: f64,
    /// Human-readable reason why this was flagged.
    pub reason: String,
    /// Suggested sources or directions for filling the gap.
    pub suggested_sources: Vec<String>,
    /// Which heuristic(s) detected this gap.
    pub detection_type: GapType,
    /// Optional page path that triggered this gap.
    pub source_page: Option<PathBuf>,
}

/// Heuristic categories that can flag a gap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GapType {
    /// No other page links to this page.
    OrphanPage,
    /// Page not updated in 30+ days.
    StalePage,
    /// Page has fewer than 100 words.
    ThinPage,
    /// Wikilink points to a non-existent page.
    BrokenWikilink,
    /// Related topics not cross-referenced.
    MissingCrossReference,
}

impl std::fmt::Display for GapType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GapType::OrphanPage => write!(f, "orphan_page"),
            GapType::StalePage => write!(f, "stale_page"),
            GapType::ThinPage => write!(f, "thin_page"),
            GapType::BrokenWikilink => write!(f, "broken_wikilink"),
            GapType::MissingCrossReference => write!(f, "missing_cross_reference"),
        }
    }
}

/// A research task generated from a knowledge gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchTask {
    /// Unique identifier for the task.
    pub id: String,
    /// Topic to research.
    pub topic: String,
    /// Priority based on confidence (high, medium, low).
    pub priority: String,
    /// Description of the gap.
    pub description: String,
    /// Suggested sources.
    pub suggested_sources: Vec<String>,
    /// Expected outcome.
    pub expected_outcome: String,
    /// Source page, if applicable.
    pub source_page: Option<String>,
}

/// Summary report from a learning loop run.
#[derive(Debug, Default)]
pub struct LearningReport {
    /// Number of gaps found.
    pub gaps_found: usize,
    /// Number of research tasks queued.
    pub research_queued: usize,
    /// Average confidence improvement estimate (0.0-1.0).
    pub confidence_improvement: f64,
    /// Breakdown of gaps by type.
    pub gaps_by_type: HashMap<String, usize>,
    /// Total pages analyzed.
    pub pages_analyzed: usize,
}

/// Cached page data extracted during scanning.
#[derive(Debug)]
struct PageInfo {
    path: PathBuf,
    title: String,
    wikilinks: Vec<String>,
    word_count: usize,
    modified_at: Option<SystemTime>,
}

/// Learning loop for knowledge gap identification and research task generation.
pub struct LearningLoop {
    stale_threshold_days: u64,
    thin_page_threshold: usize,
}

impl LearningLoop {
    /// Create a new LearningLoop with sensible defaults.
    pub fn new() -> Self {
        Self {
            stale_threshold_days: 30,
            thin_page_threshold: 100,
        }
    }

    /// Analyze a wiki directory and identify all knowledge gaps.
    pub fn analyze_gaps(wiki_dir: &Path) -> Result<Vec<KnowledgeGap>> {
        let loop_instance = Self::new();
        loop_instance._analyze_gaps(wiki_dir)
    }

    /// Queue research tasks for the identified gaps, writing markdown files to output_dir.
    pub fn queue_research(gaps: &[KnowledgeGap], output_dir: &Path) -> Result<Vec<String>> {
        fs::create_dir_all(output_dir)
            .with_context(|| format!("failed to create output dir: {}", output_dir.display()))?;

        let tasks = Self::gaps_to_tasks(gaps);
        let mut created = Vec::new();

        for task in &tasks {
            let filename = format!("research-{}.md", task.id);
            let filepath = output_dir.join(&filename);

            let content = Self::render_research_task(task)?;
            fs::write(&filepath, &content).with_context(|| {
                format!("failed to write research task: {}", filepath.display())
            })?;

            created.push(filename);
        }

        info!("Queued {} research tasks", created.len());
        Ok(created)
    }

    /// Run a full learning loop cycle: analyze gaps, queue research, return report.
    pub fn run(wiki_dir: &Path, output_dir: &Path) -> Result<LearningReport> {
        let loop_instance = Self::new();

        // Phase 1: Analyze gaps
        let gaps = loop_instance._analyze_gaps(wiki_dir)?;
        let pages_analyzed = loop_instance.scan_pages(wiki_dir).len();

        // Phase 2: Queue research
        let queued_files = Self::queue_research(&gaps, output_dir)?;

        let report = loop_instance.build_report(&gaps, &queued_files, pages_analyzed);

        info!(
            gaps_found = report.gaps_found,
            research_queued = report.research_queued,
            "learning loop complete"
        );

        Ok(report)
    }

    fn _analyze_gaps(&self, wiki_dir: &Path) -> Result<Vec<KnowledgeGap>> {
        if !wiki_dir.is_dir() {
            warn!(path = %wiki_dir.display(), "wiki directory does not exist, returning empty gaps");
            return Ok(Vec::new());
        }

        let pages = self.scan_pages(wiki_dir);
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        // Build lookup maps

        let title_set: BTreeSet<String> = pages.iter().map(|p| p.title.clone()).collect();
        let mut incoming_links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut _all_outbound_targets: BTreeSet<String> = BTreeSet::new();

        for page in &pages {
            for link_target in &page.wikilinks {
                incoming_links
                    .entry(link_target.clone())
                    .or_default()
                    .insert(page.title.clone());
                _all_outbound_targets.insert(link_target.clone());
            }
        }

        let mut gaps = Vec::new();

        gaps.extend(self.detect_orphans(&pages, &incoming_links));

        gaps.extend(self.detect_stale(&pages));
        gaps.extend(self.detect_thin(&pages));
        gaps.extend(self.detect_broken_links(&pages, &title_set));
        gaps.extend(self.detect_missing_crossrefs(&pages, &title_set));

        info!(gap_count = gaps.len(), "gap analysis complete");
        Ok(gaps)
    }

    /// Recursively scan wiki_dir for .md files and extract page metadata.
    fn scan_pages(&self, wiki_dir: &Path) -> Vec<PageInfo> {
        let mut pages = Vec::new();

        for entry in WalkDir::new(wiki_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path().to_path_buf();
            if path.extension().is_some_and(|ext| ext == "md")
                && let Ok(content) = fs::read_to_string(&path)
            {
                let wikilinks = WikiPage::extract_wikilinks(&content);
                let word_count = content.split_whitespace().count();
                let title = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let modified_at = fs::metadata(&path).ok().and_then(|m| m.modified().ok());

                pages.push(PageInfo {
                    path,
                    title,
                    wikilinks,
                    word_count,
                    modified_at,
                });
            }
        }

        pages
    }

    fn detect_orphans(
        &self,
        pages: &[PageInfo],
        incoming_links: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<KnowledgeGap> {
        pages
            .iter()
            .filter_map(|page| {
                if !page.wikilinks.is_empty() {
                    // Page links to others, but nobody links back — orphan candidate
                    let has_incoming = incoming_links.contains_key(&page.title);
                    if !has_incoming {
                        return Some(KnowledgeGap {
                            topic: format!("Orphan page: {}", page.title),
                            confidence: 0.7,
                            reason: format!(
                                "Page '{}' has outgoing links but no incoming wikilinks from other pages",
                                page.title
                            ),
                            suggested_sources: vec![format!(
                                "Find relevant pages in the wiki that should link to '{}'",
                                page.title
                            )],
                            detection_type: GapType::OrphanPage,
                            source_page: Some(page.path.clone()),
                        });
                    }
                } else {
                    // Page with no outgoing links AND no incoming links — pure orphan
                    let has_incoming = incoming_links.contains_key(&page.title);
                    if !has_incoming {
                        return Some(KnowledgeGap {
                            topic: format!("Orphan page: {}", page.title),
                            confidence: 0.5,
                            reason: format!(
                                "Page '{}' has no outgoing or incoming wikilinks",
                                page.title
                            ),
                            suggested_sources: vec![format!(
                                "Review '{}' to identify related topics and add cross-links",
                                page.title
                            )],
                            detection_type: GapType::OrphanPage,
                            source_page: Some(page.path.clone()),
                        });
                    }
                }
                None
            })
            .collect()
    }

    fn detect_stale(&self, pages: &[PageInfo]) -> Vec<KnowledgeGap> {
        let now = SystemTime::now();
        let threshold_secs = self.stale_threshold_days * 24 * 3600;

        pages
            .iter()
            .filter_map(|page| {
                page.modified_at.and_then(|modified| {
                    now.duration_since(modified)
                        .ok()
                        .filter(|d| d.as_secs() > threshold_secs)
                        .map(|d| {
                            let days_stale = d.as_secs() / 86400;
                            KnowledgeGap {
                                topic: format!("Stale page: {}", page.title),
                                confidence: 0.6,
                                reason: format!(
                                    "Page '{}' last modified {} days ago (threshold: {} days)",
                                    page.title, days_stale, self.stale_threshold_days
                                ),
                                suggested_sources: vec![format!(
                                    "Review '{}' for outdated information and update",
                                    page.title
                                )],
                                detection_type: GapType::StalePage,
                                source_page: Some(page.path.clone()),
                            }
                        })
                })
            })
            .collect()
    }

    fn detect_thin(&self, pages: &[PageInfo]) -> Vec<KnowledgeGap> {
        pages
            .iter()
            .filter_map(|page| {
                if page.word_count < self.thin_page_threshold && page.word_count > 0 {
                    Some(KnowledgeGap {
                        topic: format!("Thin page: {}", page.title),
                        confidence: 0.8,
                        reason: format!(
                            "Page '{}' has only {} words (threshold: {})",
                            page.title, page.word_count, self.thin_page_threshold
                        ),
                        suggested_sources: vec![format!(
                            "Expand '{}' with more detail, examples, and cross-references",
                            page.title
                        )],
                        detection_type: GapType::ThinPage,
                        source_page: Some(page.path.clone()),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    fn detect_broken_links(
        &self,
        pages: &[PageInfo],
        title_set: &BTreeSet<String>,
    ) -> Vec<KnowledgeGap> {
        let mut gaps = Vec::new();

        for page in pages {
            for target in &page.wikilinks {
                if !title_set.contains(target) {
                    // Check if a file exists on disk matching the target
                    let target_path = page.path.parent().map(|p| p.join(format!("{target}.md")));
                    let file_exists = target_path.as_ref().is_some_and(|p| p.exists());

                    if !file_exists {
                        gaps.push(KnowledgeGap {
                            topic: format!("Broken wikilink: [[{}]]", target),
                            confidence: 0.5,
                            reason: format!(
                                "Page '{}' contains wikilink '[[{}]]' but no page with title '{}' exists",
                                page.title, target, target
                            ),
                            suggested_sources: vec![
                                format!("Create new page '{}'", target),
                                format!(
                                    "Fix the wikilink in '{}' to point to an existing page",
                                    page.title
                                ),
                            ],
                            detection_type: GapType::BrokenWikilink,
                            source_page: Some(page.path.clone()),
                        });
                    }
                }
            }
        }

        gaps
    }

    fn detect_missing_crossrefs(
        &self,
        pages: &[PageInfo],
        title_set: &BTreeSet<String>,
    ) -> Vec<KnowledgeGap> {
        let mut gaps = Vec::new();

        // Build keyword -> pages map from content
        let mut keyword_pages: HashMap<String, Vec<&PageInfo>> = HashMap::new();
        for page in pages {
            let content_text = page.content_text();
            let words: Vec<&str> = content_text
                .split_whitespace()
                .filter(|w| w.len() > 3)
                .collect();
            let unique_words: BTreeSet<&str> = words.into_iter().collect();
            // Check each title keyword against page content
            for title in title_set {
                if title.len() > 3 && page.title != *title {
                    let title_lower = title.to_lowercase();
                    for word in &unique_words {
                        if word.to_lowercase().contains(&title_lower)
                            || title_lower.contains(&word.to_lowercase())
                        {
                            keyword_pages.entry(title.clone()).or_default().push(page);
                            break;
                        }
                    }
                }
            }
        }

        // For each keyword hit, check if the page already has a wikilink to that title
        for (target_title, referencing_pages) in &keyword_pages {
            for page in referencing_pages {
                let already_linked = page.wikilinks.contains(target_title);
                if !already_linked {
                    gaps.push(KnowledgeGap {
                        topic: format!(
                            "Missing cross-ref: {} should link to {}",
                            page.title, target_title
                        ),
                        confidence: 0.4,
                        reason: format!(
                            "Page '{}' mentions '{}', but has no wikilink to it",
                            page.title, target_title
                        ),
                        suggested_sources: vec![format!(
                            "Add a wikilink from '{}' to '[[{}]]'",
                            page.title, target_title
                        )],
                        detection_type: GapType::MissingCrossReference,
                        source_page: Some(page.path.clone()),
                    });
                }
            }
        }

        // Deduplicate by topic
        let mut seen = BTreeSet::new();
        gaps.retain(|g| seen.insert(g.topic.clone()));

        gaps
    }

    fn gaps_to_tasks(gaps: &[KnowledgeGap]) -> Vec<ResearchTask> {
        gaps.iter()
            .enumerate()
            .map(|(i, gap)| {
                let priority = if gap.confidence >= 0.7 {
                    "high"
                } else if gap.confidence >= 0.5 {
                    "medium"
                } else {
                    "low"
                };

                let expected_outcome = match gap.detection_type {
                    GapType::OrphanPage => {
                        "Integrate this page into the wiki's link graph through cross-references"
                    }
                    GapType::StalePage => {
                        "Refresh outdated content and verify accuracy"
                    }
                    GapType::ThinPage => {
                        "Expand the page to at least 100 words with meaningful content"
                    }
                    GapType::BrokenWikilink => {
                        "Resolve the broken link by creating the target page or fixing the reference"
                    }
                    GapType::MissingCrossReference => {
                        "Add appropriate cross-links between related pages"
                    }
                };

                ResearchTask {
                    id: format!("gap-{:04}", i + 1),
                    topic: gap.topic.clone(),
                    priority: priority.to_string(),
                    description: gap.reason.clone(),
                    suggested_sources: gap.suggested_sources.clone(),
                    expected_outcome: expected_outcome.to_string(),
                    source_page: gap.source_page.as_ref().map(|p| {
                        p.file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default()
                    }),
                }
            })
            .collect()
    }

    fn render_research_task(task: &ResearchTask) -> Result<String> {
        let now: DateTime<Utc> = Utc::now();
        let timestamp = now.format("%Y-%m-%dT%H:%M:%SZ");

        let sources_list = task
            .suggested_sources
            .iter()
            .map(|s| format!("- {}", s))
            .collect::<Vec<_>>()
            .join("\n");

        let source_ref = task.source_page.as_deref().unwrap_or("N/A");

        Ok(format!(
            "---\ntype: research_task\nstatus: pending\npriority: {priority}\ntopic: \"{topic}\"\ncreated: {timestamp}\nsource_page: {source_ref}\n---\n\n# Research Task: {topic}\n\n## Description\n\n{description}\n\n## Suggested Sources\n\n{sources_list}\n\n## Expected Outcome\n\n{expected_outcome}\n",
            priority = task.priority,
            topic = task.topic,
            description = task.description,
            sources_list = sources_list,
            expected_outcome = task.expected_outcome,
            source_ref = source_ref,
        ))
    }

    fn build_report(
        &self,
        gaps: &[KnowledgeGap],
        queued_files: &[String],
        pages_analyzed: usize,
    ) -> LearningReport {
        let mut gaps_by_type: HashMap<String, usize> = HashMap::new();
        let mut total_confidence: f64 = 0.0;

        for gap in gaps {
            *gaps_by_type
                .entry(gap.detection_type.to_string())
                .or_default() += 1;
            total_confidence += gap.confidence;
        }

        let avg_confidence = if gaps.is_empty() {
            0.0
        } else {
            total_confidence / gaps.len() as f64
        };

        // Confidence improvement is estimated as: if we fill all gaps, how much
        // would the average gap confidence decrease? We model this conservatively
        // as 70% of the current average (i.e., filling gaps removes ~70% of the signal).
        let confidence_improvement = if gaps.is_empty() {
            1.0
        } else {
            1.0 - (avg_confidence * 0.3)
        };

        LearningReport {
            gaps_found: gaps.len(),
            research_queued: queued_files.len(),
            confidence_improvement,
            gaps_by_type,
            pages_analyzed,
        }
    }
}

impl Default for LearningLoop {
    fn default() -> Self {
        Self::new()
    }
}

// Make the content field accessible from PageInfo
impl PageInfo {
    fn content_text(&self) -> String {
        fs::read_to_string(&self.path).unwrap_or_default()
    }
}

use async_trait::async_trait;
use rig_compose::context::{InvestigationContext, Signal};
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};

#[async_trait]
impl Skill for LearningLoop {
    fn id(&self) -> &str {
        "zen-vault-learning-loop"
    }

    fn description(&self) -> &str {
        "Identify knowledge gaps in the wiki (thin pages, orphans, broken links, missing cross-refs) and queue research tasks"
    }

    fn applies(&self, ctx: &InvestigationContext) -> bool {
        !ctx.evidence.is_empty()
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let wiki_dir = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("wiki_dir").and_then(|v| v.as_str()))
            .next()
            .map(PathBuf::from);

        let output_dir = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("output_dir").and_then(|v| v.as_str()))
            .next()
            .map(PathBuf::from);

        let (wiki_dir, output_dir) = match (wiki_dir, output_dir) {
            (Some(w), Some(o)) => (w, o),
            _ => {
                info!("LearningLoop: missing wiki_dir or output_dir in context, skipping");
                return Ok(SkillOutcome::noop());
            }
        };

        let report = Self::run(&wiki_dir, &output_dir)
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

        if report.gaps_found > 0 {
            ctx.signals.push(Signal::new("knowledge_gaps_found"));
            ctx.pending_actions
                .push(rig_compose::context::NextAction::RunSkill(
                    "zen-entity-extraction".to_string(),
                ));
        }

        ctx.evidence.push(
            rig_compose::context::Evidence::new(self.id(), "learning_loop_report").with_detail(
                serde_json::json!({
                    "gaps_found": report.gaps_found,
                    "research_queued": report.research_queued,
                    "pages_analyzed": report.pages_analyzed,
                    "gaps_by_type": report.gaps_by_type,
                    "confidence_improvement": report.confidence_improvement,
                }),
            ),
        );

        info!(
            gaps_found = report.gaps_found,
            research_queued = report.research_queued,
            "LearningLoop skill execution complete"
        );

        let delta = if report.gaps_found > 0 {
            (report.gaps_found.min(10) as f32) * 0.05
        } else {
            0.0
        };

        Ok(SkillOutcome::noop().with_delta(delta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write as _;
    use tempfile::TempDir;

    fn setup_test_wiki() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("create temp dir");
        let wiki = tmp.path().join("wiki");
        fs::create_dir_all(&wiki).expect("create wiki dir");
        (tmp, wiki)
    }

    fn write_page(wiki: &Path, name: &str, content: &str) -> PathBuf {
        let path = wiki.join(format!("{name}.md"));
        let mut f = File::create(&path).expect("create page");
        f.write_all(content.as_bytes()).expect("write page");
        path
    }

    #[test]
    fn test_detect_orphan_pages() {
        let (_tmp, wiki) = setup_test_wiki();

        // A links to B, but C has no incoming links
        write_page(&wiki, "A", "See [[B]] for details.");
        write_page(&wiki, "B", "This is page B.");
        // C is orphan: has outgoing links but nobody links to it
        write_page(&wiki, "C", "I link to [[B]] but nobody links to me.");

        let loop_instance = LearningLoop::new();
        let gaps = loop_instance._analyze_gaps(&wiki).expect("analyze");
        let orphans: Vec<_> = gaps
            .iter()
            .filter(|g| g.detection_type == GapType::OrphanPage)
            .collect();

        // C should be detected as orphan (has outgoing links, no incoming)
        assert!(
            orphans.iter().any(|g| g.topic.contains("C")),
            "Expected C to be flagged as orphan, got: {:?}",
            orphans
        );
    }

    #[test]
    fn test_detect_thin_pages() {
        let (_tmp, wiki) = setup_test_wiki();

        // Thin page: fewer than 100 words
        write_page(&wiki, "thin", "Short page with few words.");
        // Normal page: many words
        let normal_content = "word ".repeat(150);
        write_page(&wiki, "normal", &normal_content);

        let loop_instance = LearningLoop::new();
        let gaps = loop_instance._analyze_gaps(&wiki).expect("analyze");
        let thin: Vec<_> = gaps
            .iter()
            .filter(|g| g.detection_type == GapType::ThinPage)
            .collect();

        assert!(
            thin.iter().any(|g| g.topic.contains("thin")),
            "Expected thin page detection, got: {:?}",
            thin
        );
    }

    #[test]
    fn test_detect_broken_wikilinks() {
        let (_tmp, wiki) = setup_test_wiki();

        // Page A links to nonexistent pages
        write_page(&wiki, "A", "See [[NonExistent]] and [[AnotherMissing]].");

        let loop_instance = LearningLoop::new();
        let gaps = loop_instance._analyze_gaps(&wiki).expect("analyze");
        let broken: Vec<_> = gaps
            .iter()
            .filter(|g| g.detection_type == GapType::BrokenWikilink)
            .collect();

        assert_eq!(
            broken.len(),
            2,
            "Expected 2 broken links, got: {:?}",
            broken
        );
        assert!(broken.iter().any(|g| g.topic.contains("NonExistent")));
        assert!(broken.iter().any(|g| g.topic.contains("AnotherMissing")));
    }

    #[test]
    fn test_empty_knowledge_base() {
        let (_tmp, wiki) = setup_test_wiki();

        // Empty wiki directory — no .md files
        let loop_instance = LearningLoop::new();
        let gaps = loop_instance._analyze_gaps(&wiki).expect("analyze");
        assert!(gaps.is_empty(), "Expected no gaps for empty wiki");
    }

    #[test]
    fn test_nonexistent_wiki_directory() {
        let loop_instance = LearningLoop::new();
        let fake_dir = Path::new("/nonexistent/wiki/path");
        let gaps = loop_instance._analyze_gaps(fake_dir).expect("analyze");
        assert!(
            gaps.is_empty(),
            "Expected no gaps for nonexistent directory"
        );
    }

    #[test]
    fn test_queue_research_generation() {
        let tmp = TempDir::new().expect("create temp dir");
        let output = tmp.path().join("research");

        let gaps = vec![
            KnowledgeGap {
                topic: "Test gap".to_string(),
                confidence: 0.8,
                reason: "Testing".to_string(),
                suggested_sources: vec!["source1".to_string()],
                detection_type: GapType::ThinPage,
                source_page: Some(PathBuf::from("test.md")),
            },
            KnowledgeGap {
                topic: "Another gap".to_string(),
                confidence: 0.4,
                reason: "Testing 2".to_string(),
                suggested_sources: vec!["source2".to_string()],
                detection_type: GapType::BrokenWikilink,
                source_page: None,
            },
        ];

        let files = LearningLoop::queue_research(&gaps, &output).expect("queue");

        assert_eq!(files.len(), 2, "Expected 2 research task files");
        assert!(files[0].starts_with("research-"));
        assert!(files[1].starts_with("research-"));

        // Verify file content has expected structure
        let content = fs::read_to_string(output.join(&files[0])).expect("read task");
        assert!(content.contains("type: research_task"));
        assert!(content.contains("status: pending"));
        assert!(content.contains("priority: high")); // confidence 0.8 >= 0.7
        assert!(content.contains("Test gap"));
    }

    #[test]
    fn test_report_generation() {
        let tmp = TempDir::new().expect("create temp dir");
        let wiki = tmp.path().join("wiki");
        fs::create_dir_all(&wiki).expect("create wiki");
        let output = tmp.path().join("output");

        // Create some pages to analyze
        write_page(&wiki, "A", "Hello world with some content.");
        write_page(&wiki, "B", "Related to [[A]].");
        write_page(&wiki, "C", "Broken [[Missing]] link.");

        let report = LearningLoop::run(&wiki, &output).expect("run");

        assert!(report.gaps_found > 0, "Expected some gaps");
        assert!(report.research_queued > 0, "Expected some tasks queued");
        assert!(report.pages_analyzed == 3, "Expected 3 pages analyzed");
        assert!(
            report.confidence_improvement > 0.0,
            "Expected positive improvement"
        );
        assert!(
            !report.gaps_by_type.is_empty(),
            "Expected gaps_by_type to be populated"
        );
    }

    #[test]
    fn test_confidence_bounds() {
        let gap = KnowledgeGap {
            topic: "test".to_string(),
            confidence: 0.75,
            reason: "test".to_string(),
            suggested_sources: vec![],
            detection_type: GapType::ThinPage,
            source_page: None,
        };
        assert!(gap.confidence >= 0.0 && gap.confidence <= 1.0);
    }

    #[test]
    fn test_gap_type_display() {
        assert_eq!(GapType::OrphanPage.to_string(), "orphan_page");
        assert_eq!(GapType::StalePage.to_string(), "stale_page");
        assert_eq!(GapType::ThinPage.to_string(), "thin_page");
        assert_eq!(GapType::BrokenWikilink.to_string(), "broken_wikilink");
        assert_eq!(
            GapType::MissingCrossReference.to_string(),
            "missing_cross_reference"
        );
    }

    #[test]
    fn test_research_task_priority_assignment() {
        let gaps = vec![
            KnowledgeGap {
                topic: "high".to_string(),
                confidence: 0.9,
                reason: "".to_string(),
                suggested_sources: vec![],
                detection_type: GapType::ThinPage,
                source_page: None,
            },
            KnowledgeGap {
                topic: "medium".to_string(),
                confidence: 0.6,
                reason: "".to_string(),
                suggested_sources: vec![],
                detection_type: GapType::BrokenWikilink,
                source_page: None,
            },
            KnowledgeGap {
                topic: "low".to_string(),
                confidence: 0.3,
                reason: "".to_string(),
                suggested_sources: vec![],
                detection_type: GapType::OrphanPage,
                source_page: None,
            },
        ];

        let tasks = LearningLoop::gaps_to_tasks(&gaps);
        assert_eq!(tasks[0].priority, "high");
        assert_eq!(tasks[1].priority, "medium");
        assert_eq!(tasks[2].priority, "low");
    }
}
