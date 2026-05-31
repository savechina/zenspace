use clap::Subcommand;
use tracing::{debug, warn};

use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum SimilarCommands {
    /// Find similar notes via vector search
    Find {
        /// Note ID to find similarities for
        note_id: String,
        /// Number of similar notes to return
        #[arg(short, long)]
        limit: Option<usize>,
    },
}

pub fn execute_command(cmd: &SimilarCommands) -> Result<(), ZenError> {
    match cmd {
        SimilarCommands::Find { note_id, limit } => {
            let k = limit.unwrap_or(5);
            debug!("similar: note_id={} limit={}", note_id, k);

            warn!("Vector similarity search not yet implemented (requires embedding model)");
            eprintln!("⚠ Vector similarity search not yet implemented (requires embedding model)");
            eprintln!("Requested top_k: {k} for note: {note_id}");

            Ok(())
        }
    }
}
