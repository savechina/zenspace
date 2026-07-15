use clap::Subcommand;
use tracing::{debug, warn};

use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum GraphCommands {
    /// Query the notion knowledge graph
    Query {
        /// Notion name or ID
        notion: String,
        /// Maximum traversal depth
        #[arg(short, long)]
        depth: Option<u32>,
        /// Filter by relation type (e.g. "depends_on", "relates_to")
        #[arg(short, long)]
        relation_type: Option<String>,
    },
}

pub fn execute_command(cmd: &GraphCommands) -> Result<(), ZenError> {
    match cmd {
        GraphCommands::Query {
            notion,
            depth,
            relation_type,
        } => {
            let d = depth.unwrap_or(3);
            debug!(
                "graph: notion={} depth={:?} relation_type={:?}",
                notion, d, relation_type
            );

            warn!("Notion graph traversal not yet implemented (requires graph.db)");
            eprintln!("⚠ Notion graph traversal not yet implemented (requires graph.db)");
            eprintln!("Notion: {notion}, depth: {d}");
            if let Some(rt) = relation_type {
                eprintln!("Relation filter: {rt}");
            }

            Ok(())
        }
    }
}
