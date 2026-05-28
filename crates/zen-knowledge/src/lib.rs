pub mod brief;
pub mod consolidate;
pub mod entity;
pub mod ingest;
pub mod intent;
pub mod maintenance;
pub mod note;
pub mod search;
pub mod tools;
pub mod wiki;

pub use consolidate::{
    ChatImporter, Checkpoint, CheckpointManager, ConsolidationPipeline, ConsolidationPipelineInput,
    ConsolidationReport, Contradiction, ContradictionDetector, EntityExtractor, RecoveryManager,
    SourceIngester, TransactionScope, WikiCompiler,
};
pub use entity::{Entity, EntityService, EntityType, RelationType, Relationship};
pub use ingest::{
    FeedEntry, IngestResult, RssFetcher, extract_readable_content, fetch_feed, ingest_local_file,
    ingest_url,
};
pub use maintenance::{
    ChangeDetector, ComputeEmbeddings, EmbeddingResult, GapType, KnowledgeGap, LearningLoop,
    LearningReport, LintReportGenerator, LintResult, Linter, ReindexReport, Reindexer,
    ResearchTask, compute_embeddings, compute_embeddings_for_text, compute_file_checksum,
    needs_reindex, reindex_all, update_checksum,
};
pub use note::{Domain, Note, NoteService, parse_frontmatter, write_note};
pub use search::{
    GraphResult, SearchResult, SearchService, Tier1Search, Tier2Search, Tier3Search, Tier4Search,
    Tier5Search, TierSelector,
};
pub use wiki::{WikiIndex, WikiLog, WikiPage, WikiStructure};
