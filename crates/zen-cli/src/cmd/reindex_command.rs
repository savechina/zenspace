use std::path::PathBuf;

use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_knowledge::maintenance::Reindexer;

#[derive(Subcommand)]
pub enum ReindexCommands {
    /// Rebuild the knowledge index
    Run {
        /// Knowledge directory to scan (default: ~/.zen/knowledge/)
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// Preview actions without modifying anything
        #[arg(short, long)]
        dry_run: bool,
    },
}

pub fn execute_command(cmd: &ReindexCommands) -> Result<(), ZenError> {
    match cmd {
        ReindexCommands::Run { path, dry_run } => {
            let knowledge_dir = match path {
                Some(p) => p.clone(),
                None => {
                    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
                    paths.knowledge()
                },
            };

            if *dry_run {
                println!(
                    "Dry run: would scan {} for markdown files",
                    knowledge_dir.display()
                );
                return Ok(());
            }

            debug!("reindex: path={}", knowledge_dir.display());

            let reindexer = Reindexer::new();
            println!("Scanning {}...", knowledge_dir.display());

            let report = reindexer
                .reindex(&knowledge_dir)
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
        },
    }
}
