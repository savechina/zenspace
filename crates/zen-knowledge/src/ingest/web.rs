use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::extract_readable_content;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResult {
    pub source_url: String,
    pub title: String,
    pub content: String,
    pub stored_path: PathBuf,
    pub content_length: usize,
}

pub fn ingest_url(url: &str) -> Result<IngestResult> {
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .header("User-Agent", "Zen-Knowledge/0.1")
        .send()
        .with_context(|| format!("failed to fetch: {url}"))?;

    let html = resp
        .text()
        .with_context(|| "failed to read response body")?;
    let title = extract_title_from_html(&html)
        .unwrap_or_else(|| url.split('/').next_back().unwrap_or("untitled").to_string());
    let content = extract_readable_content(&html);

    Ok(IngestResult {
        source_url: url.to_string(),
        title,
        content: content.clone(),
        stored_path: PathBuf::new(),
        content_length: content.len(),
    })
}

pub fn ingest_local_file(file_path: &Path) -> Result<IngestResult> {
    let content = fs::read_to_string(file_path)
        .with_context(|| format!("failed to read file: {}", file_path.display()))?;

    let title = extract_title_from_md(&content).unwrap_or_else(|| {
        file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".to_string())
    });

    Ok(IngestResult {
        source_url: file_path.display().to_string(),
        title,
        content: content.clone(),
        stored_path: PathBuf::new(),
        content_length: content.len(),
    })
}

pub fn store_ingested(result: &IngestResult, raw_dir: &PathBuf) -> Result<PathBuf> {
    fs::create_dir_all(raw_dir)
        .with_context(|| format!("failed to create raw dir: {}", raw_dir.display()))?;

    let safe_title = slugify(&result.title);
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let ext = if result.source_url.starts_with("http") {
        "html.md"
    } else {
        "md"
    };
    let filename = format!("{timestamp}-{safe_title}.{ext}");
    let filepath = raw_dir.join(&filename);

    let frontmatter = format!(
        "---\ntype: ingested\ntitle: \"{}\"\nsource_url: {}\n---\n\n",
        result.title, result.source_url,
    );

    let content = format!("{}{}\n", frontmatter, result.content);
    let _bytes = content.len();

    fs::write(&filepath, &content)
        .with_context(|| format!("failed to write ingested content: {}", filepath.display()))?;

    info!(
        title = result.title,
        path = %filepath.display(),
        "ingested content stored"
    );

    Ok(filepath)
}

fn extract_title_from_html(html: &str) -> Option<String> {
    let start = html.find("<title>")? + "<title>".len();
    let end = html[start..].find("</title>")?;
    let title = html[start..start + end].trim();
    if title.is_empty() {
        return None;
    }
    Some(title.to_string())
}

fn extract_title_from_md(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("# ") {
            return Some(title.trim().to_string());
        }
        if let Some(title) = trimmed.strip_prefix("## ") {
            return Some(title.trim().to_string());
        }
        break;
    }
    None
}

fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_title_from_html() {
        let html = "<html><head><title>My Page</title></head><body>Content</body></html>";
        assert_eq!(extract_title_from_html(html), Some("My Page".to_string()));
    }

    #[test]
    fn test_extract_title_from_md_heading() {
        let md = "# My Document\n\nContent";
        assert_eq!(extract_title_from_md(md), Some("My Document".to_string()));
    }

    #[test]
    fn test_extract_title_from_md_no_heading() {
        let md = "Just plain content without heading";
        assert_eq!(extract_title_from_md(md), None);
    }

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }
}
