use std::fs;
use std::path::PathBuf;

use clap::Subcommand;
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
    /// Run the consolidation pipeline (inbox → wiki)
    Distill {
        /// Target pathway (reserved, not yet used)
        #[arg(short, long)]
        pathway: Option<String>,
        /// Filter by date (reserved, not yet used)
        #[arg(short, long)]
        date: Option<String>,
    },
}

pub async fn execute_command(operation: &WikiCommands) -> Result<(), ZenError> {
    match operation {
        WikiCommands::List => {
            let paths = ZenPaths::detect()?;
            list_wiki_pages(&paths.wiki().join("notions"))
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
            let db_path = paths.db().join("state.db");
            let db_client = zen_repo::SqliteClient::open_lazy(&db_path)
                .await
                .map_err(|e| ZenError::Message(format!("Failed to open database: {e}")))?;

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
            let result = linter
                .run(&wiki_dir)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            let generator = zen_vault::tindy::LintReportGenerator::new();
            let report_path = generator
                .generate(&result, &reports_dir)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("Lint completed (check: {:?}):", check);
            println!("  Orphan pages:       {}", result.orphan_pages.len());
            println!("  Broken wikilinks:   {}", result.broken_wikilinks.len());
            println!("  Stale claims:       {}", result.stale_claims.len());
            println!("  Knowledge gaps:     {}", result.knowledge_gaps.len());
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
            let summary = if trimmed.len() > 60 {
                format!("{}...", &trimmed[..57])
            } else {
                trimmed.to_string()
            };
            return Some(summary);
        }
        if trimmed == "---" {
            after_frontmatter = !after_frontmatter;
        }
    }
    None
}
