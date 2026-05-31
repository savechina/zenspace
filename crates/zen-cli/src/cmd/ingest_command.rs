use std::fs;

use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_knowledge::consolidate::SourceIngester;

#[derive(Subcommand)]
pub enum IngestCommands {
    /// Ingest a file or directory into the knowledge base
    Run {
        /// File or directory to ingest
        path: String,
    },
}

pub fn execute_command(cmd: &IngestCommands) -> Result<(), ZenError> {
    match cmd {
        IngestCommands::Run { path } => {
            debug!("ingest: path={}", path);

            let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
            let raw_dir = paths.raw();
            let source_path = std::path::Path::new(path);

            if source_path.is_file() {
                let dest = raw_dir.join(
                    source_path
                        .file_name()
                        .ok_or_else(|| ZenError::Message("invalid source filename".to_string()))?,
                );
                fs::copy(source_path, &dest)
                    .map_err(|e| ZenError::Message(format!("copy failed: {}", e)))?;
                debug!("copied {} -> {}", path, dest.display());
            } else if source_path.is_dir() {
                let dest = raw_dir.join(
                    source_path
                        .file_name()
                        .ok_or_else(|| ZenError::Message("invalid source dirname".to_string()))?,
                );
                recursive_copy(source_path, &dest)
                    .map_err(|e| ZenError::Message(format!("recursive copy failed: {}", e)))?;
                debug!("copied dir {} -> {}", path, dest.display());
            } else {
                return Err(ZenError::Message(format!(
                    "source path does not exist: {}",
                    path
                )));
            }

            let ingester = SourceIngester::new();
            let file_count = ingester
                .ingest(&raw_dir)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!(
                "Ingested {} into {}: {} files in raw/",
                path,
                raw_dir.display(),
                file_count
            );

            Ok(())
        }
    }
}

fn recursive_copy(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            recursive_copy(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
