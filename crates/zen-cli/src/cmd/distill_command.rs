use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::distill::DistillationPipeline;

#[derive(Subcommand)]
pub enum DistillCommands {
    /// Run the consolidation pipeline
    Run {
        /// Target pathway (reserved, not yet used)
        #[arg(short, long)]
        pathway: Option<String>,
        /// Filter by date (reserved, not yet used)
        #[arg(short, long)]
        date: Option<String>,
    },
}

pub fn execute_command(cmd: &DistillCommands) -> Result<(), ZenError> {
    match cmd {
        DistillCommands::Run { pathway, date } => {
            debug!("distill: pathway={:?} date={:?}", pathway, date);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let inbox_dir = paths.inbox();
            let wiki_dir = paths.wiki();

            let pipeline = DistillationPipeline::new();
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
