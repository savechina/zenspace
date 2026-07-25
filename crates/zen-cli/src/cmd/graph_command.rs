use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::search::Tier4Search;

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
    /// Add a GoalNode to the graph
    GoalAdd {
        /// Goal name
        name: String,
        /// Controllability score (0.0-1.0)
        #[arg(short, long)]
        controllability: Option<f64>,
        /// Core pursuit category
        #[arg(short, long)]
        core_pursuit: Option<String>,
        /// Deadline (ISO 8601 date)
        #[arg(long)]
        deadline: Option<String>,
    },
    /// Add a PathNode to the graph (serves a goal)
    PathAdd {
        /// Path name
        name: String,
        /// Goal ID or name this path serves
        #[arg(long)]
        serves_goal: Option<String>,
        /// Whether this is the default path
        #[arg(long, default_value_t = false)]
        is_default: bool,
    },
    /// List all GoalNodes
    GoalList,
    /// List all PathNodes
    PathList,
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

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            let results = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = zen_repo::SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {e}")))?;

                    Tier4Search
                        .search(&client, notion, d)
                        .await
                        .map_err(|e| ZenError::Message(format!("Graph traversal failed: {e}")))
                })
            })?;

            let filtered: Vec<_> = match relation_type {
                Some(rt) => results
                    .into_iter()
                    .filter(|r| r.relation == *rt)
                    .collect(),
                None => results,
            };

            if filtered.is_empty() {
                println!("No graph results for '{notion}' (depth={d}).");
                if let Some(rt) = relation_type {
                    println!("Filter applied: {rt}");
                }
                println!("Run `zen distill` to populate the knowledge graph.");
                return Ok(());
            }

            println!("Graph traversal for '{notion}' (depth={d}):");
            println!("{}", "-".repeat(80));
            println!(
                "{:<30} {:<15} {:<30} {:<10}",
                "Notion", "Relation", "Target", "Direction"
            );
            for r in &filtered {
                println!(
                    "{:<30} {:<15} {:<30} {:<10}",
                    truncate_str(&r.notion, 30),
                    r.relation,
                    truncate_str(&r.target, 30),
                    r.direction
                );
            }
            println!("\n{} relation(s)", filtered.len());

            Ok(())
        }
        GraphCommands::GoalAdd {
            name,
            controllability,
            core_pursuit,
            deadline,
        } => {
            let ctrl = controllability.unwrap_or(0.5);
            let pursuit = core_pursuit.as_deref().unwrap_or("general");
            let id = format!("goal-{}", uuid::Uuid::now_v7());

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = zen_repo::SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {}", e)))?;

                    let service = zen_vault::NotionService::new();
                    service
                        .upsert_goal_node(&client, &id, name, ctrl, pursuit, deadline.as_deref())
                        .await
                        .map_err(|e| ZenError::Message(e.to_string()))?;

                    Ok::<(), ZenError>(())
                })
            })?;

            println!("✓ Goal '{}' added (id: {})", name, id);
            Ok(())
        }
        GraphCommands::PathAdd {
            name,
            serves_goal,
            is_default,
        } => {
            let id = format!("path-{}", uuid::Uuid::now_v7());
            let goal = serves_goal.as_deref().unwrap_or("");

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = zen_repo::SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {}", e)))?;

                    let service = zen_vault::NotionService::new();
                    service
                        .upsert_path_node(&client, &id, name, goal, *is_default, 0.0, "")
                        .await
                        .map_err(|e| ZenError::Message(e.to_string()))?;

                    Ok::<(), ZenError>(())
                })
            })?;

            println!("✓ Path '{}' added (id: {})", name, id);
            Ok(())
        }
        GraphCommands::GoalList => {
            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            let goals = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = zen_repo::SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {}", e)))?;

                    let service = zen_vault::NotionService::new();
                    service
                        .load_all_goal_nodes(&client)
                        .await
                        .map_err(|e| ZenError::Message(e.to_string()))
                })
            })?;

            if goals.is_empty() {
                println!("No goals found.");
                return Ok(());
            }

            println!("{:<36} {:<30} {:<15} Deadline", "ID", "Name", "Control");
            println!("{}", "-".repeat(100));
            for (id, name, ctrl, _pursuit, deadline) in &goals {
                let dl = deadline.as_deref().unwrap_or("-");
                println!("{:<36} {:<30} {:<15.2} {}", id, name, ctrl, dl);
            }
            println!("\n{} goals", goals.len());
            Ok(())
        }
        GraphCommands::PathList => {
            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            let path_nodes = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let client = zen_repo::SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {}", e)))?;

                    let service = zen_vault::NotionService::new();
                    service
                        .load_all_path_nodes(&client)
                        .await
                        .map_err(|e| ZenError::Message(e.to_string()))
                })
            })?;

            if path_nodes.is_empty() {
                println!("No paths found.");
                return Ok(());
            }

            println!("{:<36} {:<30} {:<36} {:<10}", "ID", "Name", "Serves Goal", "Default");
            println!("{}", "-".repeat(100));
            for (id, name, serves_goal, is_default, _crowdedness, _alternatives) in &path_nodes {
                let goal_str = serves_goal.as_deref().unwrap_or("-");
                println!(
                    "{:<36} {:<30} {:<36} {:<10}",
                    id, name, goal_str, is_default
                );
            }
            println!("\n{} paths", path_nodes.len());
            Ok(())
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
