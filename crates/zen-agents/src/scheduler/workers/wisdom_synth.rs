use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::Value;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::paths::ZenPaths;
use zen_core::sanitize::InputSanitizer;
use zen_core::types::Sensitivity;
use zen_memory::belief::{Belief, SourceType};
use zen_provider::{DefaultRouter, LlmRouterExt};

use super::super::{WorkerContext, WorkerReport, ZenWorker};

pub struct WisdomSynthesizer {
    scheduled: Option<&'static str>,
}

impl WisdomSynthesizer {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for WisdomSynthesizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for WisdomSynthesizer {
    fn id(&self) -> &'static str {
        "wisdom-synth"
    }

    fn description(&self) -> &'static str {
        "Weekly LLM synthesis of reflections and beliefs into mental model and anti-pattern candidates"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 0 2 * * 7")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let reflections_dir = paths.vault().join("wiki/wisdom/reflections");
        let beliefs_dir = paths.vault().join("wiki/wisdom/beliefs");

        let reflections_text = load_all_reflections(&reflections_dir);
        if reflections_text.is_empty() {
            debug!("no reflections found, skipping wisdom synthesis");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }

        let mut beliefs = match Belief::load_all(&beliefs_dir) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "failed to load beliefs, continuing with empty beliefs");
                Vec::new()
            }
        };

        let decayed = zen_memory::belief::apply_decay_all(&mut beliefs, Utc::now());
        if decayed > 0 {
            info!(decayed, "applied decay to beliefs");
            for b in &beliefs {
                if let Err(e) = b.save(&beliefs_dir) {
                    warn!(belief_id = %b.id, error = %e, "failed to save decayed belief");
                }
            }
        }

        for b in &beliefs {
            if b.should_promote() {
                info!(
                    belief_id = %b.id,
                    proposition = %b.proposition,
                    posterior = b.posterior,
                    "belief candidate for promotion to wiki/wisdom/"
                );
            }
            if b.should_demote() {
                info!(
                    belief_id = %b.id,
                    proposition = %b.proposition,
                    posterior = b.posterior,
                    "belief candidate for demotion to archive/"
                );
            }
        }

        let router = match load_config() {
            Ok(c) => Some(DefaultRouter::from_agentic(c)),
            Err(e) => {
                warn!(error = %e, "failed to load config for LLM wisdom synthesis");
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

        let truncated_reflections = if reflections_text.len() > 8000 {
            let end = reflections_text
                .char_indices()
                .nth(8000)
                .map(|(i, _)| i)
                .unwrap_or(reflections_text.len());
            format!("{}...", &reflections_text[..end])
        } else {
            reflections_text
        };

        let sanitizer = InputSanitizer::new();
        let truncated_reflections = sanitizer.strip_dangerous_patterns(&truncated_reflections);
        let beliefs_formatted = sanitizer.strip_dangerous_patterns(&beliefs_formatted);

        let prompt = format!(
            r#"You are a wisdom synthesis engine. Analyze the following reflections and beliefs, then identify:
1. Recurring patterns that suggest mental models worth adopting
2. Anti-patterns (repeated mistakes, blind spots)
3. Positive patterns (practices that worked well and should be continued)
4. Beliefs that need more evidence (low confidence, high importance)

## Recent Reflections
{truncated_reflections}

## Current Beliefs
{beliefs_formatted}

Respond with ONLY a JSON object:
{{
  "mental_model_candidates": [
    {{"pattern": "...", "model": "...", "evidence_refs": ["reflection1", "belief1"]}}
  ],
  "anti_pattern_candidates": [
    {{"pattern": "...", "trigger": "...", "avoidance": "...", "evidence_refs": [...]}}
  ],
  "positive_pattern_candidates": [
    {{"pattern": "...", "trigger": "...", "reinforcement": "...", "evidence_refs": [...]}}
  ],
  "belief_updates": [
    {{"proposition": "...", "supports": true, "source": "self_observation", "note": "..."}}
  ]
}}"#
        );

        let response = tokio::task::spawn_blocking(move || {
            router.complete("wisdom_synthesis", &prompt, Sensitivity::Private)
        })
        .await
        .context("LLM wisdom synthesis task panicked")??;

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
            .context("failed to parse LLM wisdom synthesis response")?;

        let models = parsed["mental_model_candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let anti_patterns = parsed["anti_pattern_candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let positive_patterns = parsed["positive_pattern_candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let belief_updates = parsed["belief_updates"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let vault = paths.vault();
        let models_promoted = promote_mental_models(&vault, &models)?;
        let anti_patterns_promoted = promote_anti_patterns(&vault, &anti_patterns)?;
        let positive_patterns_promoted = promote_positive_patterns(&vault, &positive_patterns)?;

        let mut total_updates_applied = 0usize;

        for update in &belief_updates {
            let proposition = match update["proposition"].as_str() {
                Some(p) => p,
                None => continue,
            };
            let supports = update["supports"].as_bool().unwrap_or(true);
            let source_str = update["source"].as_str().unwrap_or("anonymous_internet");
            let source_type = parse_source_type(source_str);
            let note = update["note"].as_str().map(|s| s.to_string());

            if let Some(belief) = beliefs.iter_mut().find(|b| {
                b.proposition
                    .to_lowercase()
                    .contains(&proposition.to_lowercase())
                    || zen_memory::belief::slugify_proposition(&b.proposition)
                        == zen_memory::belief::slugify_proposition(proposition)
            }) {
                belief.update(supports, source_type, note);
                if let Err(e) = belief.save(&beliefs_dir) {
                    warn!(belief_id = %belief.id, error = %e, "failed to save updated belief");
                } else {
                    total_updates_applied += 1;
                    info!(
                        belief_id = %belief.id,
                        supports,
                        "belief updated from wisdom synthesis"
                    );
                }
            }
        }

        info!(
            models = models_promoted,
            anti_patterns = anti_patterns_promoted,
            positive_patterns = positive_patterns_promoted,
            belief_updates = total_updates_applied,
            "wisdom synthesis complete"
        );

        let suggestions_dir = paths.vault().join("wiki/wisdom/suggestions");
        fs::create_dir_all(&suggestions_dir)?;
        let suggestion_path = suggestions_dir.join(format!("{date_str}.md", date_str = Utc::now().format("%Y-%m-%d")));
        if let Err(e) = write_suggestions(&suggestion_path, &models, &anti_patterns, &positive_patterns, &belief_updates) {
            warn!(path = %suggestion_path.display(), error = %e, "failed to write synthesis suggestions");
        } else {
            info!(path = %suggestion_path.display(), "wisdom synthesis suggestions written");
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total_updates_applied,
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
        if path.extension().is_none_or(|ext| ext != "md") {
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

pub(crate) fn parse_source_type(s: &str) -> SourceType {
    match s {
        "self_observation" => SourceType::SelfObservation,
        "trusted_peer" => SourceType::TrustedPeer,
        "authority_book" => SourceType::AuthorityBook,
        _ => SourceType::AnonymousInternet,
    }
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn promote_mental_models(vault: &Path, candidates: &[Value]) -> Result<usize> {
    let models_dir = vault.join("wiki/wisdom/models");
    fs::create_dir_all(&models_dir)
        .with_context(|| format!("failed to create models dir: {}", models_dir.display()))?;

    let mut count = 0usize;
    let date_str = Utc::now().format("%Y-%m-%d").to_string();

    for m in candidates {
        let model_name = m["model"].as_str().unwrap_or("unknown-model");
        let pattern = m["pattern"].as_str().unwrap_or("unknown pattern");
        let refs = m["evidence_refs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let slug = slugify(model_name);
        let file_path = models_dir.join(format!("{slug}.md"));

        let content = if file_path.exists() {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            let entry = format!(
                "\n\n## Application — {date_str}\n\n- **Pattern**: {pattern}\n- Evidence: {refs}\n"
            );
            format!("{existing}{entry}")
        } else {
            format!(
                "---\nmodel: {model_name}\ndomain: wisdom-synth\nsource: self-observation\npromoted_at: {date_str}\n---\n\n# {model_name}\n\n**Pattern**: {pattern}\n\n**Evidence**: {refs}\n"
            )
        };

        fs::write(&file_path, &content)
            .with_context(|| format!("failed to write mental model: {}", file_path.display()))?;
        count += 1;
        debug!(model = model_name, path = %file_path.display(), "promoted mental model");
    }

    Ok(count)
}

fn promote_anti_patterns(vault: &Path, candidates: &[Value]) -> Result<usize> {
    let ap_dir = vault.join("wiki/wisdom/anti-patterns");
    fs::create_dir_all(&ap_dir)
        .with_context(|| format!("failed to create anti-patterns dir: {}", ap_dir.display()))?;

    let mut count = 0usize;
    let date_str = Utc::now().format("%Y-%m-%d").to_string();

    for ap in candidates {
        let pattern_name = ap["pattern"].as_str().unwrap_or("unknown-pattern");
        let trigger = ap["trigger"].as_str().unwrap_or("unknown trigger");
        let avoidance = ap["avoidance"].as_str().unwrap_or("unknown avoidance");

        let slug = slugify(pattern_name);
        let file_path = ap_dir.join(format!("{slug}.md"));

        let content = if file_path.exists() {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            let entry = format!(
                "\n\n## Observation — {date_str}\n\n- **Trigger**: {trigger}\n- **Avoidance**: {avoidance}\n"
            );
            format!("{existing}{entry}")
        } else {
            format!(
                "---\npattern: {pattern_name}\ntrigger: {trigger}\navoidance: {avoidance}\npromoted_at: {date_str}\n---\n\n# {pattern_name}\n\n**Trigger**: {trigger}\n\n**Avoidance**: {avoidance}\n"
            )
        };

        fs::write(&file_path, &content)
            .with_context(|| format!("failed to write anti-pattern: {}", file_path.display()))?;
        count += 1;
        debug!(pattern = pattern_name, path = %file_path.display(), "promoted anti-pattern");
    }

    Ok(count)
}

fn promote_positive_patterns(vault: &Path, candidates: &[Value]) -> Result<usize> {
    let pp_dir = vault.join("wiki/wisdom/positive-patterns");
    fs::create_dir_all(&pp_dir)
        .with_context(|| format!("failed to create positive-patterns dir: {}", pp_dir.display()))?;

    let mut count = 0usize;
    let date_str = Utc::now().format("%Y-%m-%d").to_string();

    for pp in candidates {
        let pattern_name = pp["pattern"].as_str().unwrap_or("unknown-pattern");
        let trigger = pp["trigger"].as_str().unwrap_or("unknown trigger");
        let reinforcement = pp["reinforcement"].as_str().unwrap_or("unknown reinforcement");

        let slug = slugify(pattern_name);
        let file_path = pp_dir.join(format!("{slug}.md"));

        let content = if file_path.exists() {
            let existing = fs::read_to_string(&file_path).unwrap_or_default();
            let entry = format!(
                "\n\n## Observation — {date_str}\n\n- **Trigger**: {trigger}\n- **Reinforcement**: {reinforcement}\n"
            );
            format!("{existing}{entry}")
        } else {
            format!(
                "---\npattern: {pattern_name}\ntrigger: {trigger}\nreinforcement: {reinforcement}\npromoted_at: {date_str}\n---\n\n# {pattern_name}\n\n**Trigger**: {trigger}\n\n**Reinforcement**: {reinforcement}\n"
            )
        };

        fs::write(&file_path, &content)
            .with_context(|| format!("failed to write positive-pattern: {}", file_path.display()))?;
        count += 1;
        debug!(pattern = pattern_name, path = %file_path.display(), "promoted positive-pattern");
    }

    Ok(count)
}

fn write_suggestions(
    path: &Path,
    models: &[Value],
    anti_patterns: &[Value],
    positive_patterns: &[Value],
    updates: &[Value],
) -> Result<()> {
    let date_str = Utc::now().format("%Y-%m-%d").to_string();
    let mut content = format!(
        "---\ndate: {date_str}\nsource: wisdom-synth\n---\n\n# Wisdom Synthesis — {date_str}\n\n"
    );

    content.push_str("## Mental Model Candidates\n\n");
    if models.is_empty() {
        content.push_str("_(no candidates identified)_\n\n");
    } else {
        for m in models {
            let pattern = m["pattern"].as_str().unwrap_or("?");
            let model = m["model"].as_str().unwrap_or("?");
            let refs = m["evidence_refs"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            content.push_str(&format!("- **{pattern}** → {model}\n  Evidence: {refs}\n"));
        }
        content.push('\n');
    }

    content.push_str("## Anti-Pattern Candidates\n\n");
    if anti_patterns.is_empty() {
        content.push_str("_(no anti-patterns identified)_\n\n");
    } else {
        for ap in anti_patterns {
            let pattern = ap["pattern"].as_str().unwrap_or("?");
            let trigger = ap["trigger"].as_str().unwrap_or("?");
            let avoidance = ap["avoidance"].as_str().unwrap_or("?");
            content.push_str(&format!(
                "- **{pattern}** (trigger: {trigger})\n  Avoidance: {avoidance}\n"
            ));
        }
        content.push('\n');
    }

    content.push_str("## Positive Pattern Candidates\n\n");
    if positive_patterns.is_empty() {
        content.push_str("_(no positive patterns identified)_\n\n");
    } else {
        for pp in positive_patterns {
            let pattern = pp["pattern"].as_str().unwrap_or("?");
            let trigger = pp["trigger"].as_str().unwrap_or("?");
            let reinforcement = pp["reinforcement"].as_str().unwrap_or("?");
            content.push_str(&format!(
                "- **{pattern}** (trigger: {trigger})\n  Reinforcement: {reinforcement}\n"
            ));
        }
        content.push('\n');
    }

    content.push_str("## Belief Updates Applied\n\n");
    if updates.is_empty() {
        content.push_str("_(no belief updates)_\n");
    } else {
        for u in updates {
            let proposition = u["proposition"].as_str().unwrap_or("?");
            let supports = u["supports"].as_bool().unwrap_or(true);
            let source = u["source"].as_str().unwrap_or("unknown");
            let status = if supports {
                "supported"
            } else {
                "contradicted"
            };
            content.push_str(&format!("- {proposition}: {status} ({source})\n"));
        }
    }

    fs::write(path, &content)
        .with_context(|| format!("failed to write suggestions: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_type_known() {
        assert_eq!(
            parse_source_type("self_observation"),
            SourceType::SelfObservation
        );
        assert_eq!(parse_source_type("trusted_peer"), SourceType::TrustedPeer);
        assert_eq!(
            parse_source_type("authority_book"),
            SourceType::AuthorityBook
        );
    }

    #[test]
    fn test_parse_source_type_unknown_defaults_anonymous() {
        assert_eq!(
            parse_source_type("random_stuff"),
            SourceType::AnonymousInternet
        );
        assert_eq!(parse_source_type(""), SourceType::AnonymousInternet);
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
    fn test_load_all_reflections_empty_dir() {
        let dir = std::path::PathBuf::from("/nonexistent/reflections/dir");
        let result = load_all_reflections(&dir);
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_all_reflections_concatenates() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("refl1.md"),
            "---\ntitle: r1\n---\nReflection one\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("refl2.md"),
            "---\ntitle: r2\n---\nReflection two\n",
        )
        .unwrap();

        let result = load_all_reflections(dir.path());
        assert!(result.contains("Reflection one"));
        assert!(result.contains("Reflection two"));
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

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Loss Aversion"), "loss-aversion");
        assert_eq!(slugify("Confirmation Bias"), "confirmation-bias");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("Sunk Cost (Fallacy)"), "sunk-cost-fallacy");
        assert_eq!(slugify("Dunning-Kruger Effect"), "dunning-kruger-effect");
    }

    #[test]
    fn test_slugify_multiple_spaces() {
        assert_eq!(slugify("  Some   Weird   Name  "), "some-weird-name");
    }

    #[test]
    fn test_promote_mental_models_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = vec![serde_json::json!({
            "pattern": "recurring planning failure",
            "model": "Loss Aversion",
            "evidence_refs": ["refl1", "belief2"]
        })];
        let count = promote_mental_models(dir.path(), &candidates).unwrap();
        assert_eq!(count, 1);

        let file = dir.path().join("wiki/wisdom/models/loss-aversion.md");
        assert!(file.exists());
        let content = fs::read_to_string(&file).unwrap();
        assert!(content.contains("model: Loss Aversion"));
        assert!(content.contains("recurring planning failure"));
    }

    #[test]
    fn test_promote_mental_models_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let models_dir = dir.path().join("wiki/wisdom/models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(
            models_dir.join("loss-aversion.md"),
            "---\nmodel: Loss Aversion\ndomain: test\nsource: seed\n---\n\n# Loss Aversion\n\nOriginal content\n",
        )
        .unwrap();

        let candidates = vec![serde_json::json!({
            "pattern": "new observation",
            "model": "Loss Aversion",
            "evidence_refs": ["refl3"]
        })];
        let count = promote_mental_models(dir.path(), &candidates).unwrap();
        assert_eq!(count, 1);

        let content = fs::read_to_string(models_dir.join("loss-aversion.md")).unwrap();
        assert!(content.contains("Original content"));
        assert!(content.contains("new observation"));
        assert!(content.contains("## Application"));
    }

    #[test]
    fn test_promote_anti_patterns_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let candidates = vec![serde_json::json!({
            "pattern": "Premature Optimization",
            "trigger": "when performance is mentioned",
            "avoidance": "measure first, optimize second",
            "evidence_refs": ["refl1"]
        })];
        let count = promote_anti_patterns(dir.path(), &candidates).unwrap();
        assert_eq!(count, 1);

        let file = dir
            .path()
            .join("wiki/wisdom/anti-patterns/premature-optimization.md");
        assert!(file.exists());
        let content = fs::read_to_string(&file).unwrap();
        assert!(content.contains("pattern: Premature Optimization"));
        assert!(content.contains("measure first, optimize second"));
    }

    #[test]
    fn test_promote_anti_patterns_appends_to_existing() {
        let dir = tempfile::tempdir().unwrap();
        let ap_dir = dir.path().join("wiki/wisdom/anti-patterns");
        fs::create_dir_all(&ap_dir).unwrap();
        fs::write(
            ap_dir.join("premature-optimization.md"),
            "---\npattern: Premature Optimization\n---\n\n# Premature Optimization\n\nOriginal\n",
        )
        .unwrap();

        let candidates = vec![serde_json::json!({
            "pattern": "Premature Optimization",
            "trigger": "coding session",
            "avoidance": "profile first"
        })];
        let count = promote_anti_patterns(dir.path(), &candidates).unwrap();
        assert_eq!(count, 1);

        let content = fs::read_to_string(ap_dir.join("premature-optimization.md")).unwrap();
        assert!(content.contains("Original"));
        assert!(content.contains("## Observation"));
        assert!(content.contains("profile first"));
    }
}
