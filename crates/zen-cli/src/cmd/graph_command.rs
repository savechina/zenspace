use clap::Subcommand;
use tracing::{debug, warn};

use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum GraphCommands {
    /// Query the entity knowledge graph
    Query {
        /// Entity name or ID
        entity: String,
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
            entity,
            depth,
            relation_type,
        } => {
            let d = depth.unwrap_or(3);
            debug!(
                "graph: entity={} depth={:?} relation_type={:?}",
                entity, d, relation_type
            );

            warn!("Entity graph traversal not yet implemented (requires graph.db)");
            eprintln!("⚠ Entity graph traversal not yet implemented (requires graph.db)");
            eprintln!("Entity: {entity}, depth: {d}");
            if let Some(rt) = relation_type {
                eprintln!("Relation filter: {rt}");
            }

            Ok(())
        }
    }
}
