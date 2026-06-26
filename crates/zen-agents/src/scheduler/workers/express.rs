use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Datelike, DateTime, Utc};
use serde_json::Value;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_memory::belief::Belief;
use zen_provider::{DefaultRouter, LlmRouterExt};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct ExpressWorker {
    scheduled: Option<&'static str>,
}

impl ExpressWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for ExpressWorker {
    fn id(&self) -> &'static str {
        "express"
    }

    fn description(&self) -> &'static str {
        "Weekly LLM expression of insights into publishable review and blog drafts (CODE Express step)"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 15 * * 6")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let reflections_dir = paths.vault().join("wiki/wisdom/reflections");
        let beliefs_dir = paths.vault().join("memories/beliefs");
        let commitments_dir = paths.vault().join("memories/commitments");
        let suggestions_dir = paths.vault().join("wiki/wisdom/suggestions");

        let reflections_text = load_all_reflections(&reflections_dir);
        let commitments_text = load_commitments_text(&commitments_dir);
        let suggestions_text = load_suggestions_text(&suggestions_dir);

        let beliefs = match Belief::load_all(&beliefs_dir) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to load beliefs, continuing with empty beliefs");
                Vec::new()
            }
        };

        if reflections_text.is_empty()
            && beliefs.is_empty()
            && commitments_text.is_empty()
            && suggestions_text.is_empty()
        {
            debug!("no source material found, skipping express step");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let router = match load_config() {
            Ok(c) => Some(DefaultRouter::from_agentic(c)),
            Err(e) => {
                warn!(error = %e, "failed to load config for express LLM");
                None
            }
        };

        let Some(router) = router else {
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        };

        let beliefs_formatted = format_beliefs_for_prompt(&beliefs);

        let combined_text = format!(
            "{reflections_text}\n{commitments_text}\n{suggestions_text}"
        );
        let truncated = if combined_text.len() > 8000 {
            let end = combined_text
                .char_indices()
                .nth(8000)
                .map(|(i, _)| i)
                .unwrap_or(combined_text.len());
            format!("{}...", &combined_text[..end])
        } else {
            combined_text
        };

        let prompt = format!(
            r#"You are a knowledge expression engine. Synthesize the following source material into a polished weekly review and a blog-ready draft.

## Source Material

### Reflections
{truncated}

### Current Beliefs
{beliefs_formatted}

### Open Commitments
{commitments_text}

### Recent Wisdom Suggestions
{suggestions_text}

## Task

Produce TWO outputs:

1. **Weekly Review** — A reflective summary of the week's learnings, organized by themes. 3-5 paragraphs. First-person voice. Honest about uncertainties.

2. **Blog Draft** — A shareable article draft distilling one key insight from the week into ~800 words. Engaging opening, clear structure, actionable takeaway. No filler.

Respond with ONLY a JSON object:
{{
  "weekly_review": "...markdown...",
  "blog_draft": "...markdown...",
  "key_insights": ["..."],
  "themes": ["..."]
}}"#
        );

        let response = tokio::task::spawn_blocking(move || {
            router.complete("express", &prompt, Sensitivity::Private)
        })
        .await
        .context("LLM express task panicked")??;

        let json_str = if let Some(start) = response.find("```json") {
            let after = &response[start + 7..];
            if let Some(end) = after.find("```") {
                &after[..end]
            } else {
                after
            }
        } else if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                &response[start..=end]
            } else {
                &response
            }
        } else {
            &response
        };

        let parsed: Value = serde_json::from_str(json_str.trim())
            .context("failed to parse LLM express response")?;

        let weekly_review = parsed["weekly_review"]
            .as_str()
            .unwrap_or("_(no weekly review generated)_")
            .to_string();
        let blog_draft = parsed["blog_draft"]
            .as_str()
            .unwrap_or("_(no blog draft generated)_")
            .to_string();
        let key_insights = parsed["key_insights"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let themes = parsed["themes"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let week_str = iso_week_string(Utc::now());
        let today_str = Utc::now().format("%Y-%m-%d").to_string();
        let output_dir = paths.vault().join("output");
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("failed to create output dir: {}", output_dir.display()))?;

        let review_path = output_dir.join(format!("weekly-review-{week_str}.md"));
        let review_content = format!(
            "---\ndate: {today_str}\nweek: {week_str}\nsource: express\n---\n\n{weekly_review}"
        );
        fs::write(&review_path, &review_content)
            .with_context(|| format!("failed to write weekly review: {}", review_path.display()))?;

        let blog_path = output_dir.join(format!("blog-draft-{week_str}.md"));
        let blog_content = format!(
            "---\ndate: {today_str}\nweek: {week_str}\nsource: express\nstatus: draft\n---\n\n{blog_draft}"
        );
        fs::write(&blog_path, &blog_content)
            .with_context(|| format!("failed to write blog draft: {}", blog_path.display()))?;

        let insights_count = key_insights.len();
        info!(
            week = %week_str,
            insights = insights_count,
            themes = themes.len(),
            "express synthesis complete"
        );

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: insights_count,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}

fn load_all_reflections(dir: &Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }
    let mut combined = String::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            let body = strip_frontmatter(&content);
            combined.push_str(&body);
            combined.push('\n');
        }
    }
    combined
}

fn strip_frontmatter(content: &str) -> String {
    let mut lines = content.lines();
    if lines.next() == Some("---") {
        let mut in_frontmatter = true;
        let mut body = String::new();
        for line in lines {
            if in_frontmatter && line.trim() == "---" {
                in_frontmatter = false;
                continue;
            }
            if !in_frontmatter {
                body.push_str(line);
                body.push('\n');
            }
        }
        body
    } else {
        content.to_string()
    }
}

fn load_commitments_text(dir: &Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };
    let mut result = String::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            let mut text = String::new();
            let mut created_at = String::new();
            for line in content.lines().take(10) {
                if let Some(v) = line.strip_prefix("text: ") {
                    text = v.trim().to_string();
                }
                if let Some(v) = line.strip_prefix("created_at: ") {
                    created_at = v.trim().to_string();
                }
            }
            if !text.is_empty() {
                result.push_str(&format!("- {text} (created {created_at})\n"));
            }
        }
    }
    result
}

fn load_suggestions_text(dir: &Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };
    let mut combined = String::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().is_some_and(|ext| ext == "md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            let body = strip_frontmatter(&content);
            combined.push_str(&body);
            combined.push('\n');
        }
    }
    combined
}

fn format_beliefs_for_prompt(beliefs: &[Belief]) -> String {
    if beliefs.is_empty() {
        return "_(no beliefs tracked yet)_".to_string();
    }
    let mut s = String::new();
    for b in beliefs {
        s.push_str(&format!(
            "- {} ({:.0}% confident, {} evidence)\n",
            b.proposition,
            b.posterior * 100.0,
            b.evidence_count
        ));
    }
    s
}

fn iso_week_string(now: DateTime<Utc>) -> String {
    let iso_week = now.iso_week();
    format!("{}-W{:02}", iso_week.year(), iso_week.week())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso_week_string() {
        let dt = Utc::now();
        let result = iso_week_string(dt);
        assert!(
            regex_is_match(&result, r"^\d{4}-W\d{2}$"),
            "iso_week_string should match YYYY-Wnn pattern, got: {result}"
        );
    }

    #[test]
    fn test_load_commitments_text_empty_dir() {
        let dir = std::path::PathBuf::from("/nonexistent/commitments/dir");
        let result = load_commitments_text(&dir);
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_commitments_text_parses() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("c1.md"),
            "---\ntext: Ship the feature\ncreated_at: 2026-06-20\nstatus: open\n---\nBody here",
        )
        .unwrap();
        fs::write(
            dir.path().join("c2.md"),
            "---\ntext: Write tests\ncreated_at: 2026-06-21\nstatus: open\n---\nMore body",
        )
        .unwrap();

        let result = load_commitments_text(dir.path());
        assert!(result.contains("Ship the feature"));
        assert!(result.contains("Write tests"));
        assert!(result.contains("created 2026-06-20"));
        assert!(result.contains("created 2026-06-21"));
    }

    #[test]
    fn test_load_suggestions_text_empty_dir() {
        let dir = std::path::PathBuf::from("/nonexistent/suggestions/dir");
        let result = load_suggestions_text(&dir);
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_suggestions_text_concatenates() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("s1.md"),
            "---\ntitle: s1\n---\nSuggestion one body\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("s2.md"),
            "---\ntitle: s2\n---\nSuggestion two body\n",
        )
        .unwrap();

        let result = load_suggestions_text(dir.path());
        assert!(result.contains("Suggestion one body"));
        assert!(result.contains("Suggestion two body"));
    }

    #[test]
    fn test_format_beliefs_for_prompt() {
        let beliefs = vec![Belief::new(
            "test-id".to_string(),
            "Rust is fast".to_string(),
            "tech".to_string(),
        )];
        let formatted = format_beliefs_for_prompt(&beliefs);
        assert!(formatted.contains("Rust is fast"));
        assert!(formatted.contains("50% confident"));
        assert!(formatted.contains("0 evidence"));
    }

    #[test]
    fn test_format_beliefs_for_prompt_empty() {
        let formatted = format_beliefs_for_prompt(&[]);
        assert!(formatted.contains("no beliefs tracked yet"));
    }

    #[test]
    fn test_strip_frontmatter() {
        let content = "---\ntitle: test\n---\nActual content here";
        let stripped = strip_frontmatter(content);
        assert_eq!(stripped.trim(), "Actual content here");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let content = "Just plain content";
        let stripped = strip_frontmatter(content);
        assert_eq!(stripped, "Just plain content");
    }

    fn regex_is_match(s: &str, pattern: &str) -> bool {
        match pattern {
            r"^\d{4}-W\d{2}$" => {
                let bytes = s.as_bytes();
                bytes.len() == 8
                    && bytes[0].is_ascii_digit()
                    && bytes[1].is_ascii_digit()
                    && bytes[2].is_ascii_digit()
                    && bytes[3].is_ascii_digit()
                    && bytes[4] == b'-'
                    && bytes[5] == b'W'
                    && bytes[6].is_ascii_digit()
                    && bytes[7].is_ascii_digit()
            }
            _ => true,
        }
    }
}
