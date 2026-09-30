use std::fs;
use std::path::PathBuf;

use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Subcommand)]
pub enum WikiCommands {
    /// List all wiki notion pages
    List,
    /// Show a wiki page by notion name
    Show {
        /// Notion name (slug or display name)
        name: String,
    },
    /// Rebuild the knowledge index (FTS5 + embeddings)
    Reindex {
        /// Knowledge directory to scan (default: workspace vault)
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Preview actions without modifying anything
        #[arg(short, long)]
        dry_run: bool,
        /// Rebuild FTS5 indexes only (resync when triggers drift), then exit
        #[arg(long, help = "Rebuild FTS5 indexes (resync when triggers drift)")]
        rebuild_fts: bool,
    },
    /// Run the knowledge lint (orphans, broken links, stale claims)
    Lint {
        /// Check name to run (reserved, not yet used)
        #[arg(short, long)]
        check: Option<String>,
    },
    /// Rebuild the gateway memory store (mv2) from md sources + session
    /// archives (requires a running gateway daemon)
    RebuildMemory,
    /// Run the consolidation pipeline (inbox → wiki)
    Distill {
        /// Target pathway (reserved, not yet used)
        #[arg(short, long)]
        pathway: Option<String>,
        /// Filter by date (reserved, not yet used)
        #[arg(short, long)]
        date: Option<String>,
    },
    /// Knowledge-processing loop (run/status/gaps/enable/disable)
    Loop {
        #[command(subcommand)]
        command: crate::cmd::loop_command::LoopCommands,
    },
    /// Export the wiki as an Agent Skills SKILL.md for external agents (E7)
    ExportSkill {
        /// Output file (default: ~/.zen/skills/zen-wiki/SKILL.md)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Machine-readable summary (written path + page count)
        #[arg(long)]
        json: bool,
    },
    /// Page-version iterations: list or restore a prior version (E5)
    Rollback {
        /// Page title, file stem, or wiki-relative path
        name: Option<String>,
        /// Restore a specific version by its unix-millis stamp
        /// (default: the latest)
        #[arg(long)]
        to: Option<i64>,
        /// List versions instead of restoring
        #[arg(long)]
        list: bool,
    },
}

pub async fn execute_command(operation: &WikiCommands) -> Result<(), ZenError> {
    match operation {
        WikiCommands::List => {
            let paths = ZenPaths::detect()?;
            list_wiki_pages(&paths.wiki().join("notions"))
        }
        WikiCommands::RebuildMemory => {
            // D3=A RPC model: the rebuild runs inside the sole-owner
            // daemon; without it there is nothing to rebuild remotely.
            let path = zen_gateway::transport::uds::default_socket_path();
            let client = zen_gateway::client::GatewayClient::connect(&path)
                .await
                .map_err(|_| {
                    ZenError::Message(format!(
                        "gateway daemon not reachable at {} — run `zen serve start` first",
                        path.display()
                    ))
                })?;
            client
                .handshake(
                    "cli-wiki-rebuild",
                    env!("CARGO_PKG_VERSION"),
                    Default::default(),
                )
                .await
                .map_err(|e| ZenError::Message(format!("gateway handshake failed: {e}")))?;
            println!("Rebuilding memory store (reindex + full session replay)...");
            let result = client
                .request("memory/rebuild", serde_json::json!({}))
                .await
                .map_err(|e| ZenError::Message(format!("memory/rebuild failed: {e}")))?;
            println!(
                "{} Rebuilt: {} files scanned, {} chunks indexed, {} index errors",
                "✅".green(),
                result["filesScanned"].as_u64().unwrap_or(0),
                result["chunksIndexed"].as_u64().unwrap_or(0),
                result["errors"].as_array().map(|a| a.len()).unwrap_or(0),
            );
            let replayed = result["replay"]["replayed"].as_u64().unwrap_or(0);
            let skipped = result["replay"]["skipped"].as_u64().unwrap_or(0);
            let last_offset = result["replay"]["lastOffset"].as_u64().unwrap_or(0);
            println!("  replayed: {replayed}, skipped: {skipped}, lastOffset: {last_offset}");
            Ok(())
        }
        WikiCommands::Show { name } => {
            let paths = ZenPaths::detect()?;
            show_wiki_page(&paths.wiki().join("notions"), name)
        }
        WikiCommands::Reindex {
            path,
            dry_run,
            rebuild_fts,
        } => {
            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");
            let db_client = zen_repo::SqliteClient::open_lazy(&db_path)
                .await
                .map_err(|e| ZenError::Message(format!("Failed to open database: {e}")))?;

            // NFC backfill (T143): legacy pre-2026-09-19 alias rows are stored
            // decomposed and can never match an equality lookup — heal them as
            // part of "rebuild derived state"; no-op on a clean table.
            let repaired = zen_repo::NotionsRepo::new(&db_client)
                .normalize_aliases_pass()
                .await
                .map_err(|e| ZenError::Message(e.to_string()))?;
            if repaired > 0 {
                println!("Normalized {repaired} legacy alias row(s) to NFC.");
            }

            if *rebuild_fts {
                let reindexer = zen_vault::tindy::Reindexer::with_client(db_client);
                println!("Rebuilding FTS5 indexes...");
                reindexer
                    .rebuild_fts_indexes()
                    .await
                    .map_err(|e| ZenError::Message(e.to_string()))?;
                println!("FTS5 indexes rebuilt.");
                return Ok(());
            }

            let knowledge_dir = match path {
                Some(p) => p.clone(),
                None => paths.vault(),
            };

            if *dry_run {
                println!(
                    "Dry run: would scan {} for markdown files",
                    knowledge_dir.display()
                );
                return Ok(());
            }

            debug!("reindex: path={}", knowledge_dir.display());

            let reindexer = zen_vault::tindy::Reindexer::with_client(db_client);
            println!("Scanning {}...", knowledge_dir.display());

            let report = reindexer
                .reindex(&knowledge_dir)
                .await
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!(
                "Updated {} files, {} unchanged",
                report.files_updated, report.files_unchanged
            );

            if !report.errors.is_empty() {
                eprintln!("\nErrors:");
                for err in &report.errors {
                    eprintln!("  - {err}");
                }
            }

            Ok(())
        }
        WikiCommands::Lint { check } => {
            debug!("lint: check={:?}", check);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let wiki_dir = paths.wiki();
            let reports_dir = PathBuf::from("reports");

            let linter = zen_vault::tindy::Linter::new();
            let okf_enabled = zen_core::config::load_config()
                .map(|c| c.agentic.compile.okf_lint_or_default())
                .unwrap_or(true);
            let mut result = linter
                .run_with_okf(&wiki_dir, okf_enabled)
                .map_err(|e| ZenError::Message(e.to_string()))?;
            let semantic_enabled = zen_core::config::load_config()
                .map(|c| c.agentic.lint.semantic_or_default())
                .unwrap_or(false);
            result.semantic_findings =
                zen_vault::tindy::scan_semantic(&wiki_dir, semantic_enabled).await;

            let generator = zen_vault::tindy::LintReportGenerator::new();
            let report_path = generator
                .generate(&result, &reports_dir)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("Lint completed (check: {:?}):", check);
            println!("  Orphan pages:       {}", result.orphan_pages.len());
            println!("  Broken wikilinks:   {}", result.broken_wikilinks.len());
            println!("  Stale claims:       {}", result.stale_claims.len());
            println!("  Knowledge gaps:     {}", result.knowledge_gaps.len());
            println!("  OKF missing fields: {}", result.okf_missing.len());
            for finding in &result.okf_missing {
                println!(
                    "    - {}: missing {}",
                    finding.page,
                    finding.missing.join(", ")
                );
            }
            println!("  Sources missing:    {}", result.sources_missing.len());
            println!("  Semantic findings:  {}", result.semantic_findings.len());
            for finding in &result.semantic_findings {
                println!(
                    "    - [{}] {}: {}",
                    kind_label(finding.kind),
                    finding.page,
                    finding.note
                );
            }
            println!("  Report saved to:    {}", report_path.display());

            Ok(())
        }
        WikiCommands::Distill { pathway, date } => {
            debug!("distill: pathway={:?} date={:?}", pathway, date);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let inbox_dir = paths.inbox();
            let wiki_dir = paths.wiki();

            let pipeline = zen_vault::distill::DistillationPipeline::new();
            let report = pipeline
                .run(&inbox_dir, &wiki_dir)
                .await
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!(
                "Distillation report ({}, pathway: {:?}, date: {:?}):",
                inbox_dir.display(),
                pathway,
                date
            );
            println!("  Notes processed:        {}", report.notes_processed);
            println!("  Entities extracted:     {}", report.entities_extracted);
            println!("  Wiki pages created:     {}", report.wiki_pages_created);
            println!("  Contradictions found:   {}", report.contradictions_found);

            Ok(())
        }
        WikiCommands::Loop { command } => crate::cmd::loop_command::execute_command(command).await,
        WikiCommands::ExportSkill { output, json } => {
            debug!("export-skill: output={:?} json={}", output, json);
            wiki_export_skill(output.as_deref(), *json)
        }
        WikiCommands::Rollback { name, to, list } => {
            debug!("rollback: name={:?} to={:?} list={}", name, to, list);
            wiki_rollback(name.as_deref(), *to, *list)
        }
    }
}

fn list_wiki_pages(wiki_dir: &std::path::Path) -> Result<(), ZenError> {
    if !wiki_dir.is_dir() {
        println!("No wiki pages found. Run `zen wiki distill` first.");
        return Ok(());
    }

    let mut pages: Vec<String> = Vec::new();
    for entry in fs::read_dir(wiki_dir)
        .map_err(|e| ZenError::Message(format!("failed to read wiki directory: {e}")))?
    {
        let entry =
            entry.map_err(|e| ZenError::Message(format!("failed to read wiki entry: {e}")))?;
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|ext| ext == "md")
            && let Some(name) = path.file_stem().and_then(|s| s.to_str())
        {
            pages.push(name.to_string());
        }
    }

    if pages.is_empty() {
        println!("No wiki pages found.");
        return Ok(());
    }

    pages.sort();
    debug!(count = pages.len(), "listing wiki pages");

    println!("Wiki Pages ({})", pages.len());
    println!("{}", "-".repeat(40));
    for name in &pages {
        let path = wiki_dir.join(format!("{name}.md"));
        let summary = get_page_summary(&path).unwrap_or_default();
        println!("  {name}");
        if !summary.is_empty() {
            println!("    {summary}");
        }
    }

    Ok(())
}

fn show_wiki_page(wiki_dir: &std::path::Path, name: &str) -> Result<(), ZenError> {
    let slug = name.to_lowercase().replace(' ', "-");
    let path = wiki_dir.join(format!("{slug}.md"));

    if !path.exists() {
        let exact_path = wiki_dir.join(format!("{name}.md"));
        if exact_path.exists() {
            print_page(&exact_path, name)?;
            return Ok(());
        }
        println!("Wiki page not found: {name}");
        return Ok(());
    }

    print_page(&path, name)
}

fn print_page(path: &std::path::Path, name: &str) -> Result<(), ZenError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ZenError::Message(format!("failed to read wiki page: {e}")))?;

    println!("# {name}\n");
    println!("{content}");
    Ok(())
}

fn get_page_summary(path: &std::path::Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    let mut after_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if after_frontmatter && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let summary: String = trimmed.chars().take(60).collect();
            let summary = if summary.len() < trimmed.len() {
                format!("{}...", summary)
            } else {
                summary
            };
            return Some(summary);
        }
        if trimmed == "---" {
            after_frontmatter = !after_frontmatter;
        }
    }
    None
}

fn kind_label(kind: zen_vault::tindy::SemanticFindingKind) -> &'static str {
    match kind {
        zen_vault::tindy::SemanticFindingKind::Contradiction => "contradiction",
        zen_vault::tindy::SemanticFindingKind::Gap => "gap",
        zen_vault::tindy::SemanticFindingKind::Stale => "stale",
        zen_vault::tindy::SemanticFindingKind::Redundant => "redundant",
    }
}

/// E5 page-version rollback surface: `zen wiki rollback --list` lists the
/// pages that have stored iterations and their versions;
/// `zen wiki rollback <page> [--to <millis>]` restores (capturing the
/// clobbered current version first, so a rollback is itself reversible).
fn wiki_rollback(name: Option<&str>, to: Option<i64>, list: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let wiki_dir = paths.wiki();
    let store = zen_vault::PageIterations::new(paths.vault().join("iterations"));

    if list || name.is_none() {
        let pages = store.list_pages();
        if pages.is_empty() {
            println!("No page iterations stored yet. They are captured automatically");
            println!("when a distill cycle overwrites an existing wiki page.");
            return Ok(());
        }
        println!("{}", "Page iterations".bold());
        for page_rel in pages {
            let page_path = wiki_dir.join(&page_rel);
            let versions = store.versions(&wiki_dir, &page_path);
            let title = page_rel
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            println!(
                "  {} ({} version{})",
                title.bold(),
                versions.len(),
                if versions.len() == 1 { "" } else { "s" }
            );
            for v in &versions {
                println!(
                    "    {}  {}",
                    v.millis,
                    chrono::DateTime::from_timestamp_millis(v.millis)
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_default(),
                );
            }
        }
        return Ok(());
    }

    let name = name.unwrap();
    let Some(page_path) = store.resolve_page(&wiki_dir, name) else {
        return Err(ZenError::Message(format!(
            "no iterations found for page '{name}'"
        )));
    };
    let mut versions = store.versions(&wiki_dir, &page_path);
    if versions.is_empty() {
        return Err(ZenError::Message(format!(
            "no iterations found for page '{name}'"
        )));
    }
    let version = match to {
        Some(millis) => versions
            .into_iter()
            .find(|v| v.millis == millis)
            .ok_or_else(|| {
                ZenError::Message(format!(
                    "no iteration {millis} for page '{name}' (see `zen wiki rollback --list`)"
                ))
            })?,
        None => versions.remove(0),
    };
    let when = chrono::DateTime::from_timestamp_millis(version.millis)
        .map(|t| t.to_rfc3339())
        .unwrap_or_default();
    store
        .restore(&wiki_dir, &version)
        .map_err(|e| ZenError::Message(e.to_string()))?;
    println!(
        "{} Restored {} to the {} version (the overwritten content was captured first).",
        "✅".green(),
        name.bold(),
        when,
    );
    Ok(())
}

/// E7 SKILL.md export: render the wiki inventory into an Agent Skills
/// file so external agents (Claude Code / Codex CLI / Gemini CLI) can
/// read the KB with zero runtime setup. Default target
/// `~/.zen/skills/zen-wiki/SKILL.md` — the directory name MUST equal the
/// frontmatter `name` (Agent Skills spec), which also makes the exported
/// skill discoverable by zen's own SkillLoader (FR-039 neutral plane).
fn wiki_export_skill(output: Option<&std::path::Path>, json: bool) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let wiki_dir = paths.wiki();
    let pages =
        zen_vault::collect_skill_pages(&wiki_dir).map_err(|e| ZenError::Message(e.to_string()))?;
    let rendered = zen_vault::render_wiki_skill_md(&pages);

    let out_path = match output {
        Some(p) => p.to_path_buf(),
        None => paths
            .skills()
            .join(zen_vault::WIKI_SKILL_NAME)
            .join(zen_vault::skill_file_name()),
    };
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ZenError::Message(format!("create {}: {e}", parent.display())))?;
    }
    zen_core::atomic_file::write_atomic(&out_path, rendered.as_bytes())
        .map_err(|e| ZenError::Message(format!("write {}: {e}", out_path.display())))?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "path": out_path.display().to_string(),
                "pages": pages.len(),
            })
        );
    } else {
        println!(
            "{} Exported {} wiki page{} to {}",
            "✅".green(),
            pages.len(),
            if pages.len() == 1 { "" } else { "s" },
            out_path.display(),
        );
    }
    Ok(())
}
