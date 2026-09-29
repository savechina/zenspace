pub mod convert;
pub mod rss;
pub mod web;

pub use convert::{is_office_extension, office_to_markdown, pdf_to_markdown};
pub use rss::{FeedEntry, RssFetcher, extract_readable_content, fetch_feed};
pub use web::{IngestResult, ingest_local_file, ingest_url};
