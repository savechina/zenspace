pub mod rss;
pub mod web;

pub use rss::{FeedEntry, RssFetcher, extract_readable_content, fetch_feed};
pub use web::{IngestResult, ingest_local_file, ingest_url};
