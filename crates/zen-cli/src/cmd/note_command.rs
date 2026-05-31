use clap::Subcommand;
use colored::Colorize;
use serde::Serialize;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_knowledge::note::NoteService;

#[derive(Subcommand)]
pub enum NoteCommands {
    /// Create a new note
    Create {
        /// Note content
        content: String,
        /// Tags (can be specified multiple times)
        #[arg(short, long)]
        tag: Vec<String>,
        /// Source identifier (defaults to "cli")
        #[arg(short, long)]
        source: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// JSON-friendly note response
#[derive(Serialize)]
struct NoteResponse {
    id: String,
    file_path: Option<String>,
    tags: Vec<String>,
    source: String,
}

pub fn execute_command(operation: &NoteCommands) -> Result<(), ZenError> {
    match operation {
        NoteCommands::Create {
            content,
            tag,
            source,
            json,
        } => {
            debug!(
                "creating note: content_len={} tags={:?} source={:?} json={}",
                content.len(),
                tag,
                source,
                json
            );

            let src = source.as_deref().unwrap_or("cli");
            let service = NoteService::new();
            let note = service
                .create_note(content, tag.clone(), src)
                .map_err(|e| ZenError::Service(e.to_string()))?;

            if *json {
                let resp = NoteResponse {
                    id: note.id.clone(),
                    file_path: note.file_path.as_ref().map(|p| p.display().to_string()),
                    tags: note.tags.clone(),
                    source: note.source.clone(),
                };
                let json_out = serde_json::to_string_pretty(&resp)
                    .map_err(|e| ZenError::Message(format!("JSON serialization failed: {e}")))?;
                println!("{json_out}");
            } else {
                println!("{} Note created", "✓".green().bold());
                println!("  ID:      {}", note.id.dimmed());
                if let Some(ref fp) = note.file_path {
                    println!("  File:    {}", fp.display().to_string().cyan());
                }
            }

            Ok(())
        }
    }
}
