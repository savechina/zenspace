use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEntry {
    pub title: String,
    pub content: String,
    pub published_date: Option<String>,
    pub link: Option<String>,
    pub feed_name: Option<String>,
}

pub struct RssFetcher {
    client: reqwest::blocking::Client,
}

impl Default for RssFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RssFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
        }
    }

    pub fn fetch_feed(&self, url: &str) -> Result<Vec<FeedEntry>> {
        info!(url, "fetching RSS feed");

        let resp = self
            .client
            .get(url)
            .header("User-Agent", "Zen-Knowledge/0.1")
            .send()
            .with_context(|| format!("failed to fetch feed: {url}"))?;

        let bytes = resp
            .bytes()
            .with_context(|| "failed to read feed response body")?;

        let xml = String::from_utf8_lossy(&bytes);
        let feed_name = extract_xml_tag(&xml, "title")
            .filter(|t| !t.is_empty())
            .map(|s| s.to_string());

        let entries: Vec<FeedEntry> = extract_xml_items(&xml)
            .iter()
            .map(|item| {
                let title = item
                    .get("title")
                    .cloned()
                    .unwrap_or_else(|| "Untitled".to_string());
                let content = item
                    .get("description")
                    .map(|c| extract_readable_content(c))
                    .unwrap_or_default();
                let published_date = item.get("pubDate").cloned();
                let link = item.get("link").cloned();

                FeedEntry {
                    title,
                    content,
                    published_date,
                    link,
                    feed_name: feed_name.clone(),
                }
            })
            .collect();

        info!(
            feed = feed_name.as_deref().unwrap_or(url),
            entry_count = entries.len(),
            "feed fetched successfully"
        );

        Ok(entries)
    }

    pub fn store_entry(&self, entry: &FeedEntry, raw_dir: &PathBuf) -> Result<PathBuf> {
        std::fs::create_dir_all(raw_dir)
            .with_context(|| format!("failed to create raw dir: {}", raw_dir.display()))?;

        let safe_title = slugify(&entry.title);
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let filename = format!("{timestamp}-{safe_title}.md");
        let filepath = raw_dir.join(&filename);

        let feed_name = entry.feed_name.as_deref().unwrap_or("rss");
        let link = entry.link.as_deref().unwrap_or("unknown");

        let frontmatter = format!(
            "---\ntype: rss_feed\ntitle: \"{}\"\nsource: {}\nlink: {}\npublished: {}\n---\n\n",
            entry.title,
            feed_name,
            link,
            entry.published_date.as_deref().unwrap_or("unknown"),
        );

        let content = format!("{}{}\n", frontmatter, entry.content);

        std::fs::write(&filepath, content)
            .with_context(|| format!("failed to write feed entry: {}", filepath.display()))?;

        Ok(filepath)
    }
}

pub fn fetch_feed(url: &str) -> Result<Vec<FeedEntry>> {
    RssFetcher::new().fetch_feed(url)
}

pub fn extract_readable_content(html: &str) -> String {
    let text = html2md::parse_html(html);
    let trimmed = text.trim();
    if trimmed.len() > 5000 {
        let truncated = &trimmed[..5000];
        let last_newline = truncated.rfind('\n').unwrap_or(4990);
        format!("{}...(truncated)", &trimmed[..last_newline])
    } else {
        trimmed.to_string()
    }
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

fn extract_xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let start = xml.find(&opening)? + opening.len();
    let end = xml[start..].find(&closing)?;
    Some(xml[start..start + end].trim())
}

fn extract_xml_items(xml: &str) -> Vec<HashMap<String, String>> {
    let mut items = Vec::new();
    let tag_pairs = ["item", "entry"];

    for tag in &tag_pairs {
        let opening = format!("<{tag}>");
        let closing = format!("</{tag}>");
        let mut pos = 0;
        while let Some(item_start) = xml[pos..].find(&opening) {
            let item_start = pos + item_start + opening.len();
            if let Some(item_end) = xml[item_start..].find(&closing) {
                let item_xml = &xml[item_start..item_start + item_end];
                let mut item_data = HashMap::new();
                for field in &[
                    "title",
                    "link",
                    "description",
                    "pubDate",
                    "summary",
                    "content",
                ] {
                    if let Some(value) = extract_xml_tag(item_xml, field) {
                        item_data.insert(field.to_string(), value.to_string());
                    }
                }
                if let Some(summary) = item_data.remove("summary") {
                    item_data
                        .entry("description".to_string())
                        .or_insert(summary.to_string());
                }
                if let Some(content) = item_data.remove("content") {
                    item_data
                        .entry("description".to_string())
                        .or_insert(content.to_string());
                }
                items.push(item_data);
                pos = item_start + item_end + closing.len();
            } else {
                break;
            }
        }
        if !items.is_empty() {
            break;
        }
    }
    items
}
