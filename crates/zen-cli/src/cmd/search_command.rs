use std::path::PathBuf;

use clap::Subcommand;
use serde::Serialize;
use tracing::debug;

use zen_core::config::load_config;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_provider::DefaultRouter;
use zen_vault::search::{SearchResult, SearchService};

#[derive(Subcommand)]
pub enum SearchCommands {
    /// Search the knowledge base
    Run {
        /// Search query
        query: String,
        /// Tier filter (1-5), auto-selected if omitted
        #[arg(short, long)]
        tier: Option<u8>,
        /// Max results (stub: not yet applied)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Domain filter
        #[arg(short, long)]
        domain: Option<String>,
        /// Output results as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct SearchOutput {
    results: Vec<SearchResultJson>,
    total: usize,
    query: String,
}

#[derive(Serialize)]
struct SearchResultJson {
    file: PathBuf,
    line: u32,
    content: String,
}

impl From<&SearchResult> for SearchResultJson {
    fn from(r: &SearchResult) -> Self {
        Self {
            file: r.file.clone(),
            line: r.line,
            content: r.content.clone(),
        }
    }
}

pub async fn execute_command(cmd: &SearchCommands) -> Result<(), ZenError> {
    match cmd {
        SearchCommands::Run {
            query,
            tier,
            limit,
            domain,
            json,
        } => {
            debug!(
                "search: query={} tier={:?} limit={:?} domain={:?}",
                query, tier, limit, domain
            );

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let base_dir = paths.vault();
            let config =
                load_config().map_err(|e| ZenError::Message(format!("Config error: {}", e)))?;
            let router = DefaultRouter::from_agentic(config);
            let service = SearchService::new(router);

            let db_path = paths.db().join("state.db");
            let client = zen_repo::SqliteClient::open(&db_path)
                .await
                .map_err(|e| ZenError::Message(format!("Database error: {}", e)))?;

            let results = service
                .search(query, &base_dir, &client, *tier, domain.as_deref())
                .await
                .map_err(|e| ZenError::Message(e.to_string()))?;

            if results.is_empty() {
                if *json {
                    let output = SearchOutput {
                        results: vec![],
                        total: 0,
                        query: query.clone(),
                    };
                    let json_str = serde_json::to_string_pretty(&output)
                        .map_err(|e| ZenError::Message(e.to_string()))?;
                    println!("{}", json_str);
                } else {
                    println!("No results found for: {}", query);
                }
                return Ok(());
            }

            if *json {
                let output = SearchOutput {
                    results: results.iter().map(SearchResultJson::from).collect(),
                    total: results.len(),
                    query: query.clone(),
                };
                let json_str = serde_json::to_string_pretty(&output)
                    .map_err(|e| ZenError::Message(e.to_string()))?;
                println!("{}", json_str);
            } else {
                for r in &results {
                    let display = r.file.display().to_string();
                    println!("{}:{} {}", display, r.line, r.content);
                }
                println!("\n{} results", results.len());
            }

            Ok(())
        }
    }
}
