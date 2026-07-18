use std::path::PathBuf;

use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_repo::SqliteClient;
use zen_vault::tindy::Reindexer;

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

pub async fn execute_command(cmd: &ReindexCommands) -> Result<(), ZenError> {
    match cmd {
        ReindexCommands::Run { path, dry_run } => {
            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
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

            let db_path = paths.db().join("state.db");
            let db_client = SqliteClient::open_lazy(&db_path)
                .await
                .map_err(|e| ZenError::Message(format!("Failed to open database: {e}")))?;

            let reindexer = Reindexer::with_client(db_client);
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
    }
}
