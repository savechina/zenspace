use std::fs;

use clap::Subcommand;
use tracing::debug;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Subcommand)]
pub enum WikiCommands {
    /// List all wiki entity pages
    List,
    /// Show a wiki page by entity name
    Show {
        /// Entity name (slug or display name)
        name: String,
    },
}

pub fn execute_command(operation: &WikiCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect()?;
    let wiki_dir = paths.wiki().join("entities");

    match operation {
        WikiCommands::List => list_wiki_pages(&wiki_dir),
        WikiCommands::Show { name } => show_wiki_page(&wiki_dir, name),
    }
}

fn list_wiki_pages(wiki_dir: &std::path::Path) -> Result<(), ZenError> {
    if !wiki_dir.is_dir() {
        println!("No wiki pages found. Run consolidation first (`zen consolidate run`).");
        return Ok(());
    }

    let mut pages: Vec<String> = Vec::new();
    for entry in fs::read_dir(wiki_dir)
        .map_err(|e| ZenError::Message(format!("failed to read wiki directory: {e}")))?
    {
        let entry =
            entry.map_err(|e| ZenError::Message(format!("failed to read wiki entry: {e}")))?;
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|ext| ext == "md")
            && let Some(name) = path.file_stem().and_then(|s| s.to_str())
        {
            pages.push(name.to_string());
        }
    }

    if pages.is_empty() {
        println!("No wiki pages found.");
        return Ok(());
    }

    pages.sort();
    debug!(count = pages.len(), "listing wiki pages");

    println!("Wiki Pages ({})", pages.len());
    println!("{}", "-".repeat(40));
    for name in &pages {
        // Read frontmatter for metadata if available
        let path = wiki_dir.join(format!("{name}.md"));
        let summary = get_page_summary(&path).unwrap_or_default();
        println!("  {name}");
        if !summary.is_empty() {
            println!("    {summary}");
        }
    }

    Ok(())
}

fn show_wiki_page(wiki_dir: &std::path::Path, name: &str) -> Result<(), ZenError> {
    let slug = name.to_lowercase().replace(' ', "-");
    let path = wiki_dir.join(format!("{slug}.md"));

    if !path.exists() {
        // Try exact name match
        let exact_path = wiki_dir.join(format!("{name}.md"));
        if exact_path.exists() {
            print_page(&exact_path, name)?;
            return Ok(());
        }
        println!("Wiki page not found: {name}");
        return Ok(());
    }

    print_page(&path, name)
}

fn print_page(path: &std::path::Path, name: &str) -> Result<(), ZenError> {
    let content = fs::read_to_string(path)
        .map_err(|e| ZenError::Message(format!("failed to read wiki page: {e}")))?;

    println!("# {name}\n");
    println!("{content}");
    Ok(())
}

fn get_page_summary(path: &std::path::Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    // Extract first non-heading, non-empty line after frontmatter
    let mut after_frontmatter = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if after_frontmatter && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let summary = if trimmed.len() > 60 {
                format!("{}...", &trimmed[..57])
            } else {
                trimmed.to_string()
            };
            return Some(summary);
        }
        if trimmed == "---" {
            after_frontmatter = !after_frontmatter;
        }
    }
    None
}
