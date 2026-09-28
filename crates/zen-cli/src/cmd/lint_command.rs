use std::path::PathBuf;

use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::tindy::{LintReportGenerator, Linter};

#[derive(Subcommand)]
pub enum LintCommands {
    /// Run the knowledge lint
    Run {
        /// Check name to run (reserved, not yet used)
        #[arg(short, long)]
        check: Option<String>,
    },
}

pub fn execute_command(cmd: &LintCommands) -> Result<(), ZenError> {
    match cmd {
        LintCommands::Run { check } => {
            debug!("lint: check={:?}", check);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let wiki_dir = paths.wiki();
            let reports_dir = PathBuf::from("reports");

            let linter = Linter::new();
            let okf_enabled = zen_core::config::load_config()
                .map(|c| c.agentic.compile.okf_lint_or_default())
                .unwrap_or(true);
            let result = linter
                .run_with_okf(&wiki_dir, okf_enabled)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            let generator = LintReportGenerator::new();
            let report_path = generator
                .generate(&result, &reports_dir)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("Lint completed (check: {:?}):", check);
            println!("  Orphan pages:       {}", result.orphan_pages.len());
            println!("  Broken wikilinks:   {}", result.broken_wikilinks.len());
            println!("  Stale claims:       {}", result.stale_claims.len());
            println!("  Knowledge gaps:     {}", result.knowledge_gaps.len());
            println!("  OKF missing fields: {}", result.okf_missing.len());
            println!("  Sources missing:    {}", result.sources_missing.len());
            println!("  Report saved to:    {}", report_path.display());

            Ok(())
        }
    }
}
