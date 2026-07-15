use std::fs;

use anyhow::{Context, Result};
use tracing::{debug, info};

use zen_core::paths::ZenPaths;
use zen_memory::Belief;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct EvidenceGatherer {
    scheduled: Option<&'static str>,
}

impl Default for EvidenceGatherer {
    fn default() -> Self {
        Self::new()
    }
}

impl EvidenceGatherer {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for EvidenceGatherer {
    fn id(&self) -> &'static str {
        "evidence-gatherer"
    }

    fn description(&self) -> &'static str {
        "Scan beliefs with low evidence count, generate research method suggestions"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 6 * * 1")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let beliefs_dir = match paths.workspace_root() {
            Some(root) => root.join("memories").join("beliefs"),
            None => {
                debug!("no workspace root configured, skipping evidence gathering");
                return Ok(WorkerReport {
                    worker_id: self.id().to_string(),
                    success: true,
                    fact_count: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        };

        let beliefs = Belief::load_all(&beliefs_dir)
            .with_context(|| format!("failed to load beliefs from: {}", beliefs_dir.display()))?;

        let weak_beliefs: Vec<&Belief> = beliefs.iter().filter(|b| b.evidence_count < 3).collect();

        if weak_beliefs.is_empty() {
            debug!("no beliefs with low evidence count found");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let suggestions_dir = match paths.workspace_root() {
            Some(root) => root.join("memories").join("research-suggestions"),
            None => unreachable!("already checked above"),
        };
        fs::create_dir_all(&suggestions_dir).with_context(|| {
            format!(
                "failed to create research-suggestions dir: {}",
                suggestions_dir.display()
            )
        })?;

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let output_path = suggestions_dir.join(format!("{today}.md"));

        let content = format_research_suggestions(&weak_beliefs, chrono::Utc::now());
        fs::write(&output_path, &content).with_context(|| {
            format!(
                "failed to write research suggestions: {}",
                output_path.display()
            )
        })?;

        info!(
            count = weak_beliefs.len(),
            path = %output_path.display(),
            "research suggestions generated"
        );

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: weak_beliefs.len(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn format_research_suggestions(beliefs: &[&Belief], now: chrono::DateTime<chrono::Utc>) -> String {
    let mut content = String::new();
    content.push_str("# Research Suggestions\n\n");
    content.push_str(&format!("Generated: {}\n\n", now.to_rfc3339()));
    content.push_str("The following beliefs have insufficient evidence (count < 3).\n");
    content.push_str("Suggested research methods are provided to strengthen each belief.\n\n");

    for belief in beliefs {
        content.push_str(&format!("## {}\n\n", belief.proposition));
        content.push_str(&format!(
            "- **Current evidence count**: {}\n",
            belief.evidence_count
        ));
        content.push_str(&format!(
            "- **Posterior**: {:.1}%\n",
            belief.posterior * 100.0
        ));
        content.push_str(&format!("- **Domain**: {}\n\n", belief.domain));
        content.push_str("### Suggested Research Methods\n\n");
        content.push_str(&suggest_methods(belief));
        content.push('\n');
    }

    content
}

fn suggest_methods(belief: &Belief) -> String {
    let mut suggestions = String::new();

    suggestions.push_str("- Review recent journal entries for direct observations on this topic\n");
    suggestions.push_str("- Search knowledge base for related notions or wiki pages\n");
    suggestions.push_str("- Conduct a targeted web search for authoritative sources\n");

    if belief.evidence_count == 0 {
        suggestions.push_str("- Run a focused session to gather initial evidence\n");
    }

    if belief.posterior < 0.3 || belief.posterior > 0.7 {
        suggestions.push_str("- Challenge the belief: actively seek contradicting evidence\n");
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_belief_file(dir: &Path, id: &str, proposition: &str, evidence_count: u32) {
        let content = format!(
            "---\nid: {}\nproposition: \"{}\"\nposterior: 0.5000\nevidence_count: {}\nweight: 1.0000\ndomain: test\ncreated_at: 2026-06-01T00:00:00Z\nlast_updated: 2026-06-01T00:00:00Z\n---\n\n# Belief: {}\n\n**Posterior**: 50.0% confident\n\n## Evidence Log\n\n_(no evidence recorded yet)_\n",
            id,
            proposition.replace('"', "\\\""),
            evidence_count,
            proposition
        );
        fs::write(dir.join(format!("{id}.md")), content).unwrap();
    }

    #[test]
    fn empty_dir_ok() {
        let worker = EvidenceGatherer::new();
        assert_eq!(worker.id(), "evidence-gatherer");
    }

    #[test]
    fn all_have_evidence_ok() {
        let dir = tempdir().unwrap();
        let beliefs_dir = dir.path().join("memories").join("beliefs");
        fs::create_dir_all(&beliefs_dir).unwrap();

        write_belief_file(&beliefs_dir, "strong-belief", "well supported", 5);
        write_belief_file(&beliefs_dir, "another-strong", "also supported", 10);

        let beliefs = Belief::load_all(&beliefs_dir).unwrap();
        let weak: Vec<&Belief> = beliefs.iter().filter(|b| b.evidence_count < 3).collect();
        assert!(weak.is_empty());
    }

    #[test]
    fn generates_suggestions() {
        let dir = tempdir().unwrap();
        let beliefs_dir = dir.path().join("memories").join("beliefs");
        fs::create_dir_all(&beliefs_dir).unwrap();

        write_belief_file(&beliefs_dir, "weak-belief", "poorly supported", 1);
        write_belief_file(&beliefs_dir, "no-evidence", "no evidence at all", 0);

        let beliefs = Belief::load_all(&beliefs_dir).unwrap();
        let weak: Vec<&Belief> = beliefs.iter().filter(|b| b.evidence_count < 3).collect();
        assert_eq!(weak.len(), 2);

        let content = format_research_suggestions(&weak, chrono::Utc::now());
        assert!(content.contains("# Research Suggestions"));
        assert!(content.contains("poorly supported"));
        assert!(content.contains("no evidence at all"));
    }

    #[test]
    fn suggestion_format() {
        let dir = tempdir().unwrap();
        let beliefs_dir = dir.path().join("memories").join("beliefs");
        fs::create_dir_all(&beliefs_dir).unwrap();

        write_belief_file(&beliefs_dir, "test-belief", "test proposition", 2);

        let beliefs = Belief::load_all(&beliefs_dir).unwrap();
        let weak: Vec<&Belief> = beliefs.iter().filter(|b| b.evidence_count < 3).collect();
        let content = format_research_suggestions(&weak, chrono::Utc::now());

        assert!(content.contains("## test proposition"));
        assert!(content.contains("Current evidence count**: 2"));
        assert!(content.contains("Posterior**: 50.0%"));
        assert!(content.contains("Domain**: test"));
        assert!(content.contains("### Suggested Research Methods"));
        assert!(content.contains("Review recent journal entries"));
    }

    #[test]
    fn worker_id() {
        let worker = EvidenceGatherer::new();
        assert_eq!(worker.id(), "evidence-gatherer");
        assert_eq!(worker.schedule(), "0 0 6 * * 1");
    }
}
