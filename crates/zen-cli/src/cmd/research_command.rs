use std::fs;
use std::path::PathBuf;

use chrono::Local;
use clap::Subcommand;
use colored::Colorize;
use serde::Serialize;
use tracing::{debug, info, warn};

use zen_core::config::load_config;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_core::types::Sensitivity;
use zen_provider::{DefaultRouter, LlmRouterExt};

use zen_knowledge::search::{SearchResult, SearchService};

#[derive(Subcommand)]
pub enum ResearchCommands {
    /// Generate a research brief on a topic
    Run {
        /// Research topic (e.g. "Rust async runtime comparison")
        topic: String,
        /// Override output path (default: ~/.zen/knowledge/wiki/research/<slug>.md)
        #[arg(short, long)]
        output: Option<String>,
        /// Output the brief as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct ResearchOutput {
    topic: String,
    file_path: String,
    created_at: String,
    summary: String,
    existing_results: usize,
}

pub fn execute_command(cmd: &ResearchCommands) -> Result<(), ZenError> {
    match cmd {
        ResearchCommands::Run {
            topic,
            output,
            json,
        } => {
            debug!(topic, "research run");

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let wiki_dir = paths.wiki();

            let existing_results = search_existing_content(topic, &wiki_dir);

            let prompt = build_research_prompt(topic, &existing_results);

            let file_path = resolve_output_path(topic, output.as_deref())?;

            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    ZenError::Message(format!("Failed to create research dir: {e}"))
                })?;
            }

            let brief = match generate_brief(topic, &prompt) {
                Ok(content) => content,
                Err(e) => {
                    warn!("LLM research unavailable, generating stub: {e}");
                    generate_stub_brief(topic)
                },
            };

            fs::write(&file_path, &brief)
                .map_err(|e| ZenError::Message(format!("Failed to write research brief: {e}")))?;

            let now = Local::now().format("%Y-%m-%d %H:%M").to_string();

            if *json {
                let output = ResearchOutput {
                    topic: topic.clone(),
                    file_path: file_path.display().to_string(),
                    created_at: now,
                    summary: extract_summary(&brief),
                    existing_results: existing_results.len(),
                };
                let json_str = serde_json::to_string_pretty(&output)
                    .map_err(|e| ZenError::Message(format!("JSON serialization failed: {e}")))?;
                println!("{json_str}");
            } else {
                println!("{} Research brief created", "✓".green().bold());
                println!("  Topic:    {}", topic.cyan().bold());
                println!("  File:     {}", file_path.display().to_string().yellow());
                println!("  Created:  {}", now.dimmed());
                if !existing_results.is_empty() {
                    println!(
                        "  Found:    {} existing content matches",
                        existing_results.len().to_string().blue().bold()
                    );
                }
                let summary = extract_summary(&brief);
                if !summary.is_empty() {
                    println!("\n  Summary:");
                    for line in summary.lines() {
                        println!("    {}", line.dimmed());
                    }
                }
            }

            Ok(())
        },
    }
}

fn search_existing_content(topic: &str, wiki_dir: &std::path::Path) -> Vec<SearchResult> {
    if !wiki_dir.exists() {
        return Vec::new();
    }

    let service = SearchService::new();
    let results = service.search(topic, wiki_dir, None);

    match results {
        Ok(results) => {
            info!(
                topic,
                match_count = results.len(),
                "searched existing content"
            );
            results
        },
        Err(e) => {
            warn!(topic, error = %e, "existing content search failed, continuing without context");
            Vec::new()
        },
    }
}

fn build_research_prompt(topic: &str, existing: &[SearchResult]) -> String {
    let existing_context = if existing.is_empty() {
        String::from(
            "No existing content was found on this topic. Create a comprehensive research brief.",
        )
    } else {
        let excerpts: Vec<String> = existing
            .iter()
            .take(10)
            .map(|r| {
                format!(
                    "  - {}:{}: {}",
                    r.file.display(),
                    r.line,
                    r.content.lines().next().unwrap_or(&r.content).trim()
                )
            })
            .collect();

        format!(
            "Found {} existing content matches. Use this as context:\n\n{}.\n\nSynthesize with this existing content while adding new insights.",
            existing.len(),
            excerpts.join("\n")
        )
    };

    format!(
        "Generate a research brief on the following topic:\n\n### Topic\n{topic}\n\n### Existing Knowledge Base Context\n{existing_context}\n\n\
         Return ONLY a markdown document with these exact sections:\n\n\
         ## Summary\n\
         A concise 2-3 paragraph overview of the topic.\n\n\
         ## Key Findings\n\
         A bulleted list of the 5-8 most important findings or insights.\n\n\
         ## Existing Content References\n\
         (Only include if existing content was found) Reference the existing pages that inform this brief.\n\n\
         ## Sources\n\
         Referenced sources with titles and URLs (if known).\n\n\
         ## Open Questions\n\
         Unresolved questions or areas needing further research.\n\n\
         Keep the brief factual, well-structured, and around 500-800 words total."
    )
}

fn generate_brief(topic: &str, prompt: &str) -> Result<String, ZenError> {
    let config = load_config()?;
    let router = DefaultRouter::from_agentic(&config);

    let llm_content = router.complete("research", prompt, Sensitivity::Public)?;

    let now = Local::now().format("%Y-%m-%d");
    let slug = topic_to_slug(topic);

    Ok(format!(
        "---\ntitle: \"{topic}\"\ndate: {now}\ntopic: \"{topic}\"\ntags: [research]\nslug: \"{slug}\"\n---\n\n\
         # Research Brief: {topic}\n\n\
         {llm_content}"
    ))
}

fn generate_stub_brief(topic: &str) -> String {
    let now = Local::now().format("%Y-%m-%d");
    let slug = topic_to_slug(topic);

    format!(
        "---\ntitle: \"{topic}\"\ndate: {now}\ntopic: \"{topic}\"\ntags: [research]\nslug: \"{slug}\"\n---\n\n\
         # Research Brief: {topic}\n\n\
         > Generated without LLM — no provider was available.\n\n\
         ## Summary\n\n\
         Research brief for: {topic}\n\n\
         No LLM provider was configured or reachable. Configure a provider in ~/.zen/config.toml \
         or set ZEN_LLM_DEFAULT_PROVIDER to generate full briefs.\n\n\
         ## Key Findings\n\n\
         - LLM provider unavailable — stub brief generated\n- Configure a provider to enable full research\n\n\
         ## Sources\n\n\
         _None yet — LLM was unavailable._\n\n\
         ## Open Questions\n\n\
         - Need to configure LLM provider\n- Topic requires actual research"
    )
}

fn topic_to_slug(topic: &str) -> String {
    topic
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' | '_' => c,
            ' ' => '-',
            _ => '-',
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<&str>>()
        .join("-")
}

fn resolve_output_path(topic: &str, override_path: Option<&str>) -> Result<PathBuf, ZenError> {
    if let Some(path) = override_path {
        return Ok(PathBuf::from(path));
    }

    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let research_dir = paths.wiki().join("research");
    let slug = topic_to_slug(topic);
    Ok(research_dir.join(format!("{slug}.md")))
}

fn extract_summary(brief: &str) -> String {
    let summary_start = match brief.find("## Summary") {
        Some(pos) => pos + "## Summary".len(),
        None => return String::new(),
    };

    let remainder = &brief[summary_start..];
    let end = remainder.find("\n## ").unwrap_or(remainder.len()).min(300);

    remainder[..end].trim().to_string()
}
