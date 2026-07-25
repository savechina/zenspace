use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_repo::{EmbeddingsRepo, SqliteClient};
use zen_vault::search::{SearchResult, Tier3Search};

#[derive(Subcommand)]
pub enum SimilarCommands {
    /// Find similar notes via vector search
    Find {
        /// Note ID to find similarities for
        note_id: String,
        /// Number of similar notes to return
        #[arg(short, long)]
        limit: Option<usize>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn execute_command(cmd: &SimilarCommands) -> Result<(), ZenError> {
    match cmd {
        SimilarCommands::Find {
            note_id,
            limit,
            json,
        } => {
            let k = limit.unwrap_or(5);
            debug!("similar: note_id={} limit={} json={}", note_id, k, json);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let db_path = paths.data().join("state.db");

            let results = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    let client = SqliteClient::open(&db_path)
                        .await
                        .map_err(|e| ZenError::Message(format!("Database error: {e}")))?;

                    let embedding = EmbeddingsRepo::new(&client)
                        .get_note_embedding(note_id)
                        .await
                        .map_err(|e| ZenError::Message(format!("Embedding lookup failed: {e}")))?;

                    let embedding = match embedding {
                        Some(e) if !e.is_empty() => e,
                        _ => {
                            return Ok::<Vec<SearchResult>, ZenError>(Vec::new());
                        }
                    };

                    Tier3Search
                        .search(&client, &embedding, k)
                        .await
                        .map_err(|e| ZenError::Message(format!("Vector search failed: {e}")))
                })
            })?;

            if results.is_empty() {
                if *json {
                    println!("[]");
                } else {
                    println!(
                        "No similar notes found for '{note_id}'. \
                         Run `zen reindex run` to populate both FTS5 and vector embeddings."
                    );
                }
                return Ok(());
            }

            if *json {
                let json_arr: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "file": r.file.display().to_string(),
                            "line": r.line,
                            "content": r.content,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json_arr).unwrap_or_default()
                );
            } else {
                println!("Top {} similar notes to '{note_id}':", results.len());
                println!("{}", "-".repeat(80));
                for (i, r) in results.iter().enumerate() {
                    println!("{:>2}. {}", i + 1, r.file.display());
                    let preview: String =
                        r.content.lines().take(2).collect::<Vec<_>>().join("\n    ");
                    if !preview.is_empty() {
                        println!("    {preview}");
                    }
                }
            }

            Ok(())
        }
    }
}
