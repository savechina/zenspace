pub mod chat_import;
pub mod checkpoint;
pub mod contradiction;
pub mod notion_extraction;
pub mod pipeline;
pub mod recovery;
pub mod source_ingest;
pub mod transaction;
pub mod wiki_compile;

pub use chat_import::ChatImporter;
pub use checkpoint::{Checkpoint, CheckpointManager};
pub use contradiction::{Contradiction, ContradictionDetector};
pub use notion_extraction::NotionExtractor;
pub use pipeline::{ConsolidationPipeline, ConsolidationPipelineInput, ConsolidationReport};
pub use recovery::RecoveryManager;
pub use source_ingest::SourceIngester;
pub use transaction::TransactionScope;
pub use wiki_compile::WikiCompiler;

pub struct Consolidate;
